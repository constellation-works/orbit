use clap::Args;
use orbit_cmd::registry_runtime::{
    RegisteredRuntimeFactory, global_root_for, workspace_runtime_binding,
};
use orbit_cmd::{MigrateCommands, MigrateStatus, migrate_dry_run_at};
use orbit_core::application::agents::{ClaimedEffortBackfill, DispatchEffortBackfill};
use orbit_core::application::codebase::{DIFF_STATS_BACKFILL_DEFAULT_LIMIT, DiffStatsBackfill};
use orbit_core::application::job::TaskCrewBackfill;
use orbit_core::composition::WorkspaceAuthorityAdoption;
use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::json;

use crate::command::{Block, CommandOut, Execute, Payload};

// ORB-10012: source provenance for the versioned migration surface.
/// Inspect or apply pending `.orbit/` workspace upgrades.
///
/// By default, this command lists pending upgrades without applying them.
/// `--dry-run` explicitly requests the same inspection, while `--confirm`
/// applies the pending upgrades. Orbit also applies required workspace
/// upgrades automatically when it opens a workspace. `--adopt-authority`
/// records which generation authority protects a workspace that has
/// recorded none, without applying anything.
#[derive(Args)]
#[command(about = "Apply or inspect pending .orbit layout and store schema migrations")]
pub struct MigrateCommand {
    /// Apply pending migrations
    #[arg(long, conflicts_with = "dry_run")]
    pub confirm: bool,
    /// Explicitly request the default non-destructive inspection mode
    #[arg(long, conflicts_with_all = ["confirm", "backfill_diff_stats", "backfill_task_crews", "backfill_dispatch_effort", "backfill_claimed_effort"])]
    pub dry_run: bool,
    /// Apply pending migrations, then record diff stats for done tasks that landed before they were recorded
    #[arg(long)]
    pub backfill_diff_stats: bool,
    /// Most done tasks one diff-stats backfill records (1-5000)
    #[arg(
        long,
        requires = "backfill_diff_stats",
        default_value_t = DIFF_STATS_BACKFILL_DEFAULT_LIMIT,
        value_name = "N"
    )]
    pub limit: usize,
    /// Apply pending migrations, then set each task's crew to the crew its latest delivery selected (latest local delivery run or claimed-leaf settlement), where they differ
    #[arg(long, conflicts_with = "backfill_diff_stats")]
    pub backfill_task_crews: bool,
    /// Apply pending migrations, then record the reasoning effort each agent invocation without a recorded dispatch identity was launched with, read from its audited launch command, so agent scorecards stop showing it as effort unknown. Invocations whose launch cannot be matched, or whose matching launches disagree, stay unknown; running it again changes nothing
    #[arg(long, conflicts_with_all = ["backfill_diff_stats", "backfill_task_crews"])]
    pub backfill_dispatch_effort: bool,
    /// On a pull follower: apply pending migrations, then report to the owner the reasoning effort and implementing model of each claimed leaf this host settled, read from this host's own records, so the owner's agent scorecards stop showing those attempts as effort unknown or model unknown. Run --backfill-dispatch-effort here first when this host's invocations predate dispatch identity. The owner never overwrites an effort or model it holds; running it again records nothing new
    #[arg(long, conflicts_with_all = ["backfill_diff_stats", "backfill_task_crews", "backfill_dispatch_effort"])]
    pub backfill_claimed_effort: bool,
    /// Record the generation authority this command runs under (its home directory, --root or ORBIT_ROOT; PATH must name it) as the one protecting a workspace that has recorded none. Stop every orbit process using the workspace first; nothing is migrated
    #[arg(
        long,
        value_name = "PATH",
        conflicts_with_all = ["confirm", "dry_run", "backfill_diff_stats", "backfill_task_crews", "backfill_dispatch_effort", "backfill_claimed_effort"]
    )]
    pub adopt_authority: Option<std::path::PathBuf>,
}

impl MigrateCommand {
    /// `--dry-run` dispatches before the runtime bootstrap in `main.rs`:
    /// opening a runtime would auto-apply the very migrations being listed.
    pub fn execute_without_runtime(
        self,
        root_override: Option<&std::path::Path>,
        workspace_selector: Option<&str>,
    ) -> CommandOut {
        let (global_root, orbit_dir) = match workspace_selector
            .map(str::trim)
            .filter(|selector| !selector.is_empty())
        {
            Some(selector) => {
                let global_root = global_root_for(root_override)?;
                let selected =
                    RegisteredRuntimeFactory::resolve_workspace_selector(&global_root, selector)?;
                // Validate the checkout-local identity without opening a
                // runtime: missing or malformed identity is a re-homing
                // boundary, not permission to inspect a different root.
                workspace_runtime_binding(&selected.workspace, &selected.checkout)?;
                (global_root, selected.checkout.orbit_dir)
            }
            None => {
                let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                let Some(resolved) =
                    RegisteredRuntimeFactory::try_resolve_initialized_roots(&cwd, root_override)?
                else {
                    return Err(OrbitError::WorkspaceError(
                        "no initialized orbit workspace found from the current directory; run `orbit init` first"
                            .to_string(),
                    ));
                };
                let global_root =
                    RegisteredRuntimeFactory::resolve_bootstrap_roots_for_cwd(&cwd, root_override)?
                        .global_root;
                (global_root, resolved.shared_root)
            }
        };
        if let Some(claimed) = self.adopt_authority.as_deref() {
            return adoption_payload(&RegisteredRuntimeFactory::adopt_workspace_authority(
                &global_root,
                &orbit_dir,
                claimed,
            )?);
        }
        let status = migrate_dry_run_at(&global_root, &orbit_dir)?;

        // `--dry-run` exits nonzero when there is anything pending, and a
        // failing command has no payload — stdout carries records only
        // (spec §5). The readout is the diagnostic that explains the exit
        // code, so on the failing paths it goes to stderr alongside it.
        // A workspace newer than this binary is not necessarily unusable:
        // when every extra migration is additive it opens read-only
        // (ORB-12434), and reporting that is a successful inspection.
        if status.newer_than_binary() && !status.forward_compatible_only() {
            eprintln!("{}", status_trailer(&status));
            return Err(OrbitError::Migration(format!(
                "workspace '{}' was written by a newer orbit than this binary supports; \
                 upgrade orbit",
                status.orbit_dir.display()
            )));
        }
        let pending = status.pending_total();
        if pending > 0 {
            eprintln!("{}", status_trailer(&status));
            return Err(OrbitError::Execution(format!(
                "{pending} migration(s) pending; run `orbit migrate --confirm` to apply"
            )));
        }
        status_payload(&status, true)
    }
}

impl MigrateCommand {
    /// Whether this invocation opens the runtime, which applies pending
    /// migrations: `--confirm`, or a backfill that needs the current schema.
    pub fn applies(&self) -> bool {
        self.confirm
            || self.backfill_diff_stats
            || self.backfill_task_crews
            || self.backfill_dispatch_effort
            || self.backfill_claimed_effort
    }
}

impl Execute for MigrateCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        if self.backfill_diff_stats {
            return backfill_payload(&runtime.backfill_landed_diff_stats(self.limit)?);
        }
        if self.backfill_task_crews {
            return task_crew_backfill_payload(&runtime.backfill_admitted_task_crews()?);
        }
        if self.backfill_dispatch_effort {
            return dispatch_effort_backfill_payload(&runtime.backfill_dispatch_effort()?);
        }
        if self.backfill_claimed_effort {
            return claimed_effort_backfill_payload(&runtime.backfill_claimed_leaf_effort()?);
        }
        // Apply path: opening the runtime (before this command ran) already
        // applied everything pending; report the outcome.
        let status = runtime.migrate_status()?;
        status_payload(&status, false)
    }
}

fn adoption_payload(adoption: &WorkspaceAuthorityAdoption) -> CommandOut {
    let doc = json!({
        "workspace_authority": {
            "orbit_dir": adoption.orbit_dir.to_string_lossy(),
            "authority": adoption.authority.to_string_lossy(),
            "evidence": adoption.evidence.as_str(),
            "recorded": adoption.recorded,
            "layout_version": adoption.layout_version,
            "fenced": adoption
                .fenced
                .iter()
                .map(|authority| authority.to_string_lossy())
                .collect::<Vec<_>>(),
        }
    });
    let text = if adoption.recorded {
        format!(
            "workspace {} now records generation authority {} (evidence: {}); its pending \
             layout migrations apply under that authority once its clients quiesce",
            adoption.orbit_dir.display(),
            adoption.authority.display(),
            adoption.evidence.as_str()
        )
    } else {
        format!(
            "workspace {} already records generation authority {}; nothing changed",
            adoption.orbit_dir.display(),
            adoption.authority.display()
        )
    };
    Ok(Payload::blocks(doc, vec![Block::text(text)]).into())
}

fn backfill_payload(report: &DiffStatsBackfill) -> CommandOut {
    let doc = json!({ "diff_stats_backfill": report });
    let mut text = format!(
        "diff stats backfill: {} recorded, {} skipped as unresolvable (landed commit not in this \
         checkout), {} without a landing, {} examined",
        report.recorded, report.unresolvable, report.no_landing, report.examined
    );
    if report.remaining > 0 {
        text.push_str(&format!(
            "\n{} done tasks left past --limit {}; run it again to continue",
            report.remaining, report.limit
        ));
    }
    Ok(Payload::blocks(doc, vec![Block::text(text)]).into())
}

fn task_crew_backfill_payload(report: &TaskCrewBackfill) -> CommandOut {
    let doc = json!({ "task_crew_backfill": report });
    let mut text = format!(
        "task crew backfill in workspace {}: {} changed of {} tasks with a delivery crew selection",
        report.workspace, report.changed, report.examined
    );
    for change in &report.tasks {
        text.push_str(&format!(
            "\n  {}: {} -> {}",
            change.task_id,
            change.from.as_deref().unwrap_or("(none)"),
            change.to
        ));
    }
    Ok(Payload::blocks(doc, vec![Block::text(text)]).into())
}

fn dispatch_effort_backfill_payload(report: &DispatchEffortBackfill) -> CommandOut {
    let doc = json!({ "dispatch_effort_backfill": report });
    let mut text = format!(
        "dispatch effort backfill in workspace {}: {} recorded of {} invocations without a \
         dispatch identity; {} left unknown with no matching launch, {} with launches that \
         disagree",
        report.workspace,
        report.recorded,
        report.examined,
        report.no_launch_event,
        report.conflicting
    );
    for (effort, count) in &report.recorded_by_effort {
        text.push_str(&format!("\n  {effort}: {count}"));
    }
    if report.already_recorded > 0 {
        text.push_str(&format!(
            "\n{} recorded by another writer while this ran",
            report.already_recorded
        ));
    }
    if report.launch_only_recorded > 0 || report.launch_only_unmatched > 0 {
        text.push_str(&format!(
            "\n{} launch records recorded, one per launch, for runs whose invocation never completed\
             \n{} left unrecorded: no single step provably contained the launch\
             \n{} left unrecorded: the launch named no provider\
             \n{} left unrecorded: the launch's effort sources disagreed or were unreadable",
            report.launch_only_recorded,
            report.launch_only_no_step,
            report.launch_only_no_provider,
            report.launch_only_unreadable
        ));
    }
    text.push_str(
        "\nunits left unknown stay \"effort unknown\": invocations with no matching launch \
         (including launches before the 2026-09-10 audit argv or with no surviving launch \
         audit), launches no single step provably contained, attempts a follower ran whose \
         follower reported no effort, at settlement or later through its own `orbit migrate \
         --backfill-claimed-effort`, and runs absent from this store. Effort is never inferred \
         from crew configuration.",
    );
    Ok(Payload::blocks(doc, vec![Block::text(text)]).into())
}

fn claimed_effort_backfill_payload(report: &ClaimedEffortBackfill) -> CommandOut {
    let doc = json!({ "claimed_effort_backfill": report });
    let mut text = format!(
        "claimed leaf effort backfill in workspace {}: {} settled claims examined; {} recorded \
         on the owner, {} already held there; models: {} recorded, {} already held",
        report.workspace,
        report.examined,
        report.recorded,
        report.replayed,
        report.models_recorded,
        report.models_replayed
    );
    for (count, why) in [
        (
            report.effort_unknown,
            "left effort unknown: this host holds no implementing invocation or launch record of \
             the leaf with a recorded effort",
        ),
        (
            report.no_leaf_run,
            "left effort unknown: this host holds no record of the claim's leaf run",
        ),
        (
            report.conflicting,
            "refused: the owner holds a different effort or model, which stands",
        ),
        (
            report.models_owner_unsupported,
            "had their effort recorded but their model ignored: the owner's Orbit predates late \
             model reports; upgrade it and run this again",
        ),
        (
            report.owner_unsupported,
            "refused unread: the owner's Orbit predates late effort reports; upgrade it and run \
             this again",
        ),
        (
            report.refused,
            "refused by the owner (see each row's detail)",
        ),
        (
            report.owner_unreachable,
            "not sent: the owner was unreachable; run this again",
        ),
    ] {
        if count > 0 {
            text.push_str(&format!("\n  {count} {why}"));
        }
    }
    text.push_str(
        "\nattempts that stay \"effort unknown\" on the owner: leaves this follower holds no \
         matching invocation or launch record of (run `orbit migrate --backfill-dispatch-effort` \
         here first when its invocations predate dispatch identity), leaves whose run record \
         this follower no longer holds, and leaves of followers that never run this backfill. \
         Effort and model are never inferred from a crew name or configuration.",
    );
    Ok(Payload::blocks(doc, vec![Block::text(text)]).into())
}

fn status_payload(status: &MigrateStatus, dry_run: bool) -> CommandOut {
    let doc = json!({
        "orbit_dir": status.orbit_dir.to_string_lossy(),
        "binary_version": status.binary_version,
        "dry_run": dry_run,
        "layout": {
            "current": status.layout_version,
            "supported": status.layout_supported,
            "pending": status.pending_layout.iter().map(|m| json!({
                "version": m.version,
                "name": m.name,
                "description": m.description,
            })).collect::<Vec<_>>(),
        },
        "schema": {
            "current": status.schema_version,
            "supported": status.schema_supported,
            "pending": status.pending_schema.iter().map(|m| json!({
                "version": m.version,
                "name": m.name,
            })).collect::<Vec<_>>(),
        },
        "applied_layout": status.applied_layout.iter().map(|m| json!({
            "version": m.version,
            "name": m.name,
        })).collect::<Vec<_>>(),
        "up_to_date": status.pending_total() == 0 && !status.newer_than_binary(),
        "forward_compatible": {
            "read_only": status.forward_compatible_only() && !status.forward_compatible_writable(),
            "writable": status.forward_compatible_writable(),
            "layout": status.layout_forward_compatible.as_ref().map(|open| json!({
                "state_version": open.state_version,
                "supported_version": open.supported_version,
                "min_reader_version": open.min_reader_version,
                "writable": open.writable,
            })),
            "schema": status.schema_forward_compatible.as_ref().map(|open| json!({
                "state_version": open.state_version,
                "supported_version": open.supported_version,
                "min_reader_version": open.min_reader_version,
                "writable": open.writable,
            })),
        },
    });

    // A fixed-shape status readout, not a result set: every component is named
    // even when the two version columns happen to agree.
    use crate::output::table::{Column, Table};
    let mut table = Table::new(vec![
        Column::new("COMPONENT").fixed(),
        Column::new("CURRENT").fixed(),
        Column::new("SUPPORTED").fixed(),
    ])
    .keep_all_columns();
    table.add_row(vec![
        "workspace layout".to_string(),
        status.layout_version.to_string(),
        status.layout_supported.to_string(),
    ]);
    table.add_row(vec![
        "store schema".to_string(),
        status.schema_version.to_string(),
        status.schema_supported.to_string(),
    ]);

    Ok(Payload::blocks(
        doc,
        vec![Block::table(table), Block::notice(status_trailer(status))],
    )
    .into())
}

/// The prose under the version table: where the workspace is, what applied on
/// open, and what is still pending.
fn status_trailer(status: &MigrateStatus) -> String {
    let mut trailer = format!(
        "\nworkspace: {} (orbit {})",
        status.orbit_dir.display(),
        status.binary_version
    );
    for applied in &status.applied_layout {
        trailer.push_str(&format!(
            "\napplied on open: layout v{} ({})",
            applied.version, applied.name
        ));
    }

    for forward in [
        status.layout_forward_compatible.as_ref(),
        status.schema_forward_compatible.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        trailer.push_str(&format!("\nforward-compatible: {forward}"));
    }
    if status.forward_compatible_writable() {
        trailer.push_str(
            "\n\nThis workspace is newer than this binary, by migrations older binaries keep \
             writing through: commands read and write it as usual.",
        );
    } else if status.forward_compatible_only() {
        trailer.push_str(
            "\n\nThis workspace is newer than this binary, by read-compatible migrations: \
             read-only commands work and writes are refused. Upgrade orbit to write to it.",
        );
    }

    // The caller emits the refusal error for a workspace newer than the binary,
    // so the readout stops here rather than advising a migration that will not
    // run.
    if !status.newer_than_binary() {
        if status.pending_total() == 0 {
            trailer.push_str(&format!(
                "\n\n{}",
                crate::output::color::text(
                    "Workspace is up to date.",
                    crate::output::color::Role::Ok
                )
            ));
        } else {
            trailer.push_str("\n\nPending migrations:");
            for pending in &status.pending_layout {
                trailer.push_str(&format!(
                    "\n  layout v{} ({}) — {}",
                    pending.version, pending.name, pending.description
                ));
            }
            for pending in &status.pending_schema {
                trailer.push_str(&format!(
                    "\n  schema v{} ({})",
                    pending.version, pending.name
                ));
            }
            trailer.push_str(&format!(
                "\n\nRun `orbit migrate --confirm` to apply (migrations also auto-apply on workspace open).\n\
                 Before a major upgrade, consider backing up workspace state first:\n\
                 cp -a {orbit_dir} {orbit_dir}.bak",
                orbit_dir = status.orbit_dir.display()
            ));
        }
    }
    trailer
}
