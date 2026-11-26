//! Frozen schema steps for `orbit.db`, one module per table, with
//! `MIGRATIONS` as the one ordered step list.

mod admission_forecasts;
mod admission_reservations;
mod agent_executions;
mod agent_task_facts;
mod application_consents;
mod assistance_work;
mod audit_events;
mod automation_condition_states;
mod backlog_forecasts;
mod baseline;
mod codebase_task_facts;
mod crew_memory_revisions;
mod crew_performance_snapshot;
mod delivery_runs_stamp;
mod drain_intents;
mod evolution_records;
mod executor_census;
mod experiment_admissions;
mod experiment_events;
mod experiments;
mod feature_ledgers;
mod feature_schema_meta;
mod friction_records;
mod host_resource_minutes;
mod hosts;
mod hub_registry_metadata;
mod introspect;
mod invocations;
mod job_run_id_allocations;
mod job_run_states;
mod job_run_steps;
mod job_run_tasks;
mod job_runs;
mod landed_diff_stats;
mod learnings_index;
mod lease_notices;
mod notices;
mod operation_grants;
mod plugins;
mod provider_account_limit_observations;
mod provider_crew_outages;
mod provider_limit_observations;
mod routine_breakers;
mod routine_cursors;
mod run_resource_minutes;
mod script_grant;
mod task_commit_journal;
mod task_coordination_rows;
mod task_lock_denials;
mod task_reservations;
mod v2_audit_events;
mod worker_resource_limits;
mod workspace_ownership;

use super::ledger;
use crate::contract::{
    ForwardCompatibleOpen, MigrationCompatibility, StateComponent, evaluate_newer_state,
};
#[cfg(test)]
use introspect::{table_exists, table_has_column};
pub(crate) use orbit_common::storage::sqlite::ledger::applied_migrations;
pub use orbit_common::storage::sqlite::ledger::{
    AppliedMigration, LedgerStamp, LedgeredDatabase, Migration, current_schema_version,
    evaluate_newer_database, run_database_migrations,
};
use orbit_common::storage::sqlite::sqlite_error;
use orbit_common::{OrbitError, StorageLayer};
use rusqlite::Connection;
use std::path::Path;

/// Stable ordered registry of store-database migrations. Append-only:
/// never renumber or edit an entry that has shipped. Its body and shape-defining
/// helpers are frozen too: use only version-private code and the schema-neutral
/// primitives in `introspect`. Later shapes require later ledger entries.
/// A version-stamped data backfill may use current domain code so a later entry
/// can re-derive it (v16/v18 actor identity). The freeze-in-place extraction of
/// private helper copies preserves shipped behavior, including v13/v15 defensive
/// recreation; removing those compatibility paths requires separate approval.
/// Every prefix is guarded by the store schema golden (`make goldens`).
// L-0083: Preserve every shipped migration entry across reverts and history rewrites.
pub(crate) const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "baseline",
        compat: MigrationCompatibility::Additive,
        apply: baseline::apply_baseline_schema,
    },
    Migration {
        version: 2,
        name: "learnings_index_workspace_scope",
        // Drops and reshapes `learnings_index`, which binaries without it query.
        compat: MigrationCompatibility::Breaking,
        apply: learnings_index::apply_learning_index_workspace_scope,
    },
    Migration {
        version: 3,
        name: "flat_crew_model",
        compat: MigrationCompatibility::Additive,
        apply: job_runs::apply_flat_crew_model,
    },
    Migration {
        version: 4,
        name: "job_run_archive_stage",
        compat: MigrationCompatibility::Additive,
        apply: job_runs::apply_job_run_archive_stage,
    },
    Migration {
        version: 5,
        name: "host_registry_core",
        compat: MigrationCompatibility::Additive,
        apply: hosts::apply_host_registry_core,
    },
    Migration {
        version: 6,
        name: "workspace_coordination_projections",
        compat: MigrationCompatibility::Additive,
        apply: workspace_ownership::apply_workspace_coordination_projections,
    },
    Migration {
        version: 7,
        name: "trusted_mcp_audit_provenance",
        compat: MigrationCompatibility::Additive,
        apply: audit_events::apply_trusted_mcp_audit_provenance,
    },
    Migration {
        version: 8,
        name: "hub_registry_metadata",
        // Every host-registry write must bump `registry_revision` in the same
        // transaction; an older writer of those tables leaves it stale.
        compat: MigrationCompatibility::ReadCompatible,
        apply: hub_registry_metadata::apply_hub_registry_metadata,
    },
    Migration {
        version: 9,
        name: "feature_schema_ledger",
        compat: MigrationCompatibility::Additive,
        apply: feature_schema_meta::apply_feature_schema_ledger,
    },
    // ORB-10367: carries the invocation telemetry columns
    // (`cache_create_1h_tokens`, `provider_cost_usd`) to databases that
    // recorded the v1 baseline before those columns were added to it.
    Migration {
        version: 10,
        name: "invocation_telemetry_columns",
        compat: MigrationCompatibility::Additive,
        apply: invocations::apply_invocation_telemetry_columns,
    },
    // ADR-0287: baseline v1 is frozen; schema added later always advances
    // through an append-only ledger entry.
    Migration {
        version: 11,
        name: "routine_scheduler_schema",
        compat: MigrationCompatibility::Additive,
        apply: routine_cursors::apply_routine_scheduler_schema,
    },
    // ORB-10680: hub friction records leave the Markdown tree for the
    // host-global store, keyed by `(workspace_id, friction_id)`.
    Migration {
        version: 12,
        name: "friction_records_sqlite",
        // Friction records move into the store once, behind an import marker;
        // an older binary keeps writing the Markdown tree the newer one no
        // longer reads, and can reuse friction IDs.
        compat: MigrationCompatibility::ReadCompatible,
        apply: friction_records::apply_friction_records_schema,
    },
    // ORB-10709 / ADR-0352: `task_reservations` gains the coordination
    // dimension that separates worker file reservations from the exclusive
    // workspace claim, plus the claim's bearer token.
    Migration {
        version: 13,
        name: "workspace_claim_scope",
        // Adds the coordination dimension to `task_reservations`: a binary
        // without it reads an exclusive workspace claim as a file reservation.
        compat: MigrationCompatibility::Breaking,
        apply: task_reservations::apply_workspace_claim_scope,
    },
    // ORB-10736: keep the shipped learning migrations above intact, then
    // retire their projections explicitly for both upgraded and fresh stores.
    Migration {
        version: 14,
        name: "remove_native_learning_subsystem",
        // Drops the learning tables older binaries still query.
        compat: MigrationCompatibility::Breaking,
        apply: learnings_index::apply_remove_native_learning_subsystem,
    },
    Migration {
        version: 15,
        name: "invocation_audit_context",
        compat: MigrationCompatibility::Additive,
        apply: audit_events::apply_invocation_audit_context,
    },
    // ORB-10888: `role` alone conflates agent families, model strings, system
    // markers, and unattributed markers. The canonical actor projection lands
    // beside it and is backfilled for existing rows.
    Migration {
        version: 16,
        name: "audit_actor_identity",
        // The actor columns are derived from `role` when a row is written and
        // never re-derived; an older writer leaves them NULL, which newer
        // reads count as unattributed.
        compat: MigrationCompatibility::ReadCompatible,
        apply: audit_events::apply_audit_actor_identity,
    },
    // ORB-10890: the untrusted half of attribution. An MCP client started from
    // its own config cannot satisfy the managed-run trust boundary, so its
    // self-declared identity lands beside `role` rather than in it.
    Migration {
        version: 17,
        name: "audit_self_reported_actor",
        compat: MigrationCompatibility::Additive,
        apply: audit_events::apply_audit_self_reported_actor,
    },
    // Alias map v2: `fable` became a family rule so versioned Fable labels
    // resolve to `claude`. Rows stamped with the old map are re-derived.
    Migration {
        version: 18,
        name: "audit_actor_alias_v2",
        // An older writer keeps stamping rows with the v1 alias map, and
        // nothing re-derives them afterwards.
        compat: MigrationCompatibility::ReadCompatible,
        apply: audit_events::apply_audit_actor_alias_v2,
    },
    // The run listing orders by `created_at DESC, run_id` per workspace; the
    // existing indexes cover `(workspace_id, job_id, scheduled_at)` and
    // `(workspace_id, state)`, so every dashboard page scanned and sorted a
    // workspace's whole run history.
    Migration {
        version: 19,
        name: "job_runs_created_index",
        compat: MigrationCompatibility::Additive,
        apply: job_runs::apply_job_runs_created_index,
    },
    // Every window filter (`ts >= ? AND ts < ?`) and the newest-first
    // listing (`ORDER BY ts DESC, id DESC LIMIT n`) over `invocations` had
    // only the job-run and activity indexes to work with, so each was a
    // full scan plus a temp sort.
    Migration {
        version: 20,
        name: "invocations_ts_index",
        compat: MigrationCompatibility::Additive,
        apply: invocations::apply_invocations_ts_index,
    },
    // ORB-12528: the durable commit decision that lets one task transition,
    // its history, a reservation, and dependent coordination rows be
    // published as a single outcome across the bundle files and this
    // database. An older binary still reads correctly by ignoring both
    // tables.
    Migration {
        version: 21,
        name: "task_commit_journal",
        // Newer task writes publish through this journal and its coordination
        // rows under a partition lock; an older writer bypasses both and can
        // admit a task the newer binary has claimed.
        compat: MigrationCompatibility::ReadCompatible,
        apply: task_commit_journal::apply_task_commit_journal,
    },
    Migration {
        version: 22,
        name: "execution_provenance",
        compat: MigrationCompatibility::Additive,
        apply: job_runs::apply_execution_provenance,
    },
    // ORB-12725: `caller_host_id`/`process_host_id` carry a machine's display
    // name, so they are renamed to `caller_machine_name`/`process_machine_name`
    // with the rest of the host -> machine vocabulary. Breaking: an older
    // binary selects the retired column names by name and would fail at the
    // first audit read rather than silently losing attribution.
    Migration {
        version: 23,
        name: "audit_machine_name_columns",
        compat: MigrationCompatibility::Breaking,
        apply: audit_events::apply_audit_machine_name_columns,
    },
    // Plugin standard phase 1: the `plugins` table beside `tools`, plus the
    // audit columns carrying plugin name, version and manifest digest.
    Migration {
        version: 24,
        name: "plugins_and_audit_plugin_provenance",
        compat: MigrationCompatibility::Additive,
        apply: plugins::apply_plugins_and_audit_plugin_provenance,
    },
    // Plugin standard phase 2: the grant set a plugin call ran under.
    Migration {
        version: 25,
        name: "audit_plugin_grants",
        compat: MigrationCompatibility::Additive,
        apply: audit_events::apply_audit_plugin_grants,
    },
    // Operation mode was removed on 2026-09-21 (ORB-12772): the grant and
    // recovery-ledger tables its `operation` feature migration created go
    // with it. Breaking: an older binary reads `operation_grants` at handoff
    // commit and would fail there rather than silently skipping the check.
    Migration {
        version: 26,
        name: "remove_operation_mode",
        compat: MigrationCompatibility::Breaking,
        apply: operation_grants::apply_remove_operation_mode,
    },
    // Plugin standard phase 4: the Orbit version a plugin's conformance
    // goldens last passed on (`orbit plugin test`).
    Migration {
        version: 27,
        name: "plugin_certified_orbit_version",
        // A reinstall must clear the certification when the manifest changes;
        // an older upsert keeps it for bytes that were never tested.
        compat: MigrationCompatibility::ReadCompatible,
        apply: plugins::apply_plugin_certified_orbit_version,
    },
    // Plugin standard: the SHA-256 of the archive a digest-pinned `https://`
    // plugin source was installed from, so `orbit plugin doctor` can report a
    // pin whose digest has since moved without going back to the network.
    Migration {
        version: 28,
        name: "plugin_archive_digest",
        // A reinstall must overwrite the archive digest; an older upsert keeps
        // the previous archive's digest and hides drift from doctor.
        compat: MigrationCompatibility::ReadCompatible,
        apply: plugins::apply_plugin_archive_digest,
    },
    // Friction re-homing: the workspace that owns a friction recorded in
    // another one, so curation can finish with a durable disposition.
    Migration {
        version: 29,
        name: "friction_rehome_target",
        compat: MigrationCompatibility::Additive,
        apply: friction_records::apply_friction_rehome_target,
    },
    // Plugin secrets: the names of the declared secrets a plugin call
    // delivered, never their values.
    Migration {
        version: 30,
        name: "audit_plugin_secrets",
        compat: MigrationCompatibility::Additive,
        apply: audit_events::apply_audit_plugin_secrets,
    },
    // Plugin secret rotation: each secret a backend asked to rotate, by
    // name, and whether the update was applied or refused — never a value.
    Migration {
        version: 31,
        name: "audit_plugin_secret_updates",
        compat: MigrationCompatibility::Additive,
        apply: audit_events::apply_audit_plugin_secret_updates,
    },
    // The plugin broker: whether a call was brokered for a sandboxed agent,
    // and the peer PID that asked.
    Migration {
        version: 32,
        name: "audit_brokered_call",
        compat: MigrationCompatibility::Additive,
        apply: audit_events::apply_audit_brokered_call,
    },
    // Run ids were probed only against live `job_runs` rows, so archiving or
    // deleting a run freed its id for the next submission in that minute while
    // automation keys, audit rows and parent dispatch records still named it.
    Migration {
        version: 33,
        name: "job_run_id_allocations",
        // An older allocator ignores the reservations and can mint a freed id
        // again, handing a stale reference an unrelated run.
        compat: MigrationCompatibility::ReadCompatible,
        apply: job_run_id_allocations::apply_job_run_id_allocations,
    },
    // Per-job newest-first reads sorted every run the job ever had, and the
    // retry children read scanned the workspace for want of an index that
    // orders by `created_at`. Both are pure `CREATE INDEX IF NOT EXISTS`,
    // which an older binary ignores.
    Migration {
        version: 34,
        name: "job_runs_job_created_and_retry_indexes",
        compat: MigrationCompatibility::Additive,
        apply: job_runs::apply_job_runs_job_created_and_retry_indexes,
    },
    // Plugin standard: the build record of a plugin built from a
    // commit-pinned `git+` source under `--allow-build`
    // (`docs/design/plugins/3_install_time_build.md` §3.6).
    Migration {
        version: 35,
        name: "plugin_build_record",
        // A reinstall must overwrite the build record; an older upsert keeps
        // the previous build's record beside a tree no build produced.
        compat: MigrationCompatibility::ReadCompatible,
        apply: plugins::apply_plugin_build_record,
    },
    // Run ids are only unique within a workspace, yet invocations were keyed
    // by run id alone, so a run-id collision across workspaces read the other
    // workspace's invocations into run detail and reliability counts.
    Migration {
        version: 36,
        name: "invocation_workspace_scope",
        // An older writer records invocations without a workspace, and no
        // workspace-scoped read matches those rows.
        compat: MigrationCompatibility::ReadCompatible,
        apply: invocations::apply_invocation_workspace_scope,
    },
    Migration {
        version: 37,
        name: "audit_tool_call_index",
        compat: MigrationCompatibility::Additive,
        apply: audit_events::apply_audit_tool_call_index,
    },
    // Every run listing walked each run's pipeline-state overflow chain to
    // reach the columns `ALTER TABLE` had appended after it. The state moves
    // to a 1:1 side table and the column is dropped.
    Migration {
        version: 38,
        name: "job_run_states",
        // Drops `job_runs.pipeline_state_json`, which binaries without this
        // migration read and write for every run's state.
        compat: MigrationCompatibility::Breaking,
        apply: job_run_states::apply_job_run_states,
    },
    // Recency-ordered run pages (the dashboard's aggregate run list) sorted
    // every run of the workspace for want of an index over the recency
    // expression. Pure `CREATE INDEX IF NOT EXISTS`, which an older binary
    // ignores and SQLite maintains on its writes.
    Migration {
        version: 39,
        name: "job_runs_recency_index",
        compat: MigrationCompatibility::Additive,
        apply: job_runs::apply_job_runs_recency_index,
    },
    // ORB-14695: the host's provider usage limits. A new table only, which an
    // older binary never reads, so drains on older binaries keep writing.
    Migration {
        version: 40,
        name: "provider_limit_observations",
        compat: MigrationCompatibility::Additive,
        apply: provider_limit_observations::apply_provider_limit_observations,
    },
    // ORB-14696: a provider's own usage-window reading on the same rows. New
    // nullable or defaulted columns only, so older binaries keep writing.
    Migration {
        version: 41,
        name: "provider_limit_readings",
        compat: MigrationCompatibility::Additive,
        apply: provider_limit_observations::apply_provider_limit_readings,
    },
    // ORB-14699: the provider an invocation ran on, beside its agent. One new
    // nullable column, so older binaries keep inserting.
    Migration {
        version: 42,
        name: "invocation_provider",
        compat: MigrationCompatibility::Additive,
        apply: invocations::apply_invocation_provider,
    },
    // Windowed reads of one v2 event type (the audit summary's policy
    // denials) had no index over both the type and the window. Index-only,
    // which an older binary neither names nor needs.
    Migration {
        version: 43,
        name: "v2_audit_event_type_ts_index",
        compat: MigrationCompatibility::Additive,
        apply: v2_audit_events::apply_v2_audit_event_type_ts_index,
    },
    // Per-minute CPU and memory of each delivery run's worker scope, for
    // auto-concurrency profiles. A new table only, which an older binary never
    // names, so drains on older binaries keep writing.
    Migration {
        version: 44,
        name: "run_resource_minutes",
        compat: MigrationCompatibility::Additive,
        apply: run_resource_minutes::apply_run_resource_minutes,
    },
    // The host's CPU and memory per wall-clock minute, the ground truth for
    // the forecast. A new table only, as v44.
    Migration {
        version: 45,
        name: "host_resource_minutes",
        compat: MigrationCompatibility::Additive,
        apply: host_resource_minutes::apply_host_resource_minutes,
    },
    Migration {
        version: 46,
        name: "job_run_step_lock_wait",
        compat: MigrationCompatibility::Additive,
        apply: job_run_steps::apply_job_run_step_lock_wait,
    },
    // Admissions decided under the host admission lock whose runs may not
    // exist yet, so every drain's forecast counts them. A new table only, as
    // v44.
    Migration {
        version: 47,
        name: "admission_reservations",
        compat: MigrationCompatibility::Additive,
        apply: admission_reservations::apply_admission_reservations,
    },
    // Each drain admission pass's forecast and decisions, checked against
    // `host_resource_minutes` by the forecast accuracy report. A new table
    // only, as v44.
    Migration {
        version: 48,
        name: "admission_forecasts",
        compat: MigrationCompatibility::Additive,
        apply: admission_forecasts::apply_admission_forecasts,
    },
    // The automation, review and local-pull feature ledgers, which features
    // applied lazily on first open, run to completion here and freeze; later
    // changes to their tables are store entries. Each feature keeps its
    // `feature_schema_meta` rows and `feature.compat.<feature>` record, from
    // which a binary without this entry decides the feature as before.
    Migration {
        version: 49,
        name: "absorb_feature_ledgers",
        compat: MigrationCompatibility::Additive,
        apply: feature_ledgers::apply_absorb_feature_ledgers,
    },
    Migration {
        version: 50,
        name: "pull_forecast_decisions",
        compat: MigrationCompatibility::Additive,
        apply: admission_forecasts::apply_pull_forecast_decisions,
    },
    Migration {
        version: 51,
        name: "worker_cpu_accounting",
        compat: MigrationCompatibility::Additive,
        apply: run_resource_minutes::apply_worker_cpu_accounting,
    },
    // ORB-16379: the crews this host's provider was unavailable for. A new
    // table only, which an older binary never reads, so drains on older
    // binaries keep writing.
    Migration {
        version: 52,
        name: "provider_crew_outages",
        compat: MigrationCompatibility::Additive,
        apply: provider_crew_outages::apply_provider_crew_outages,
    },
    Migration {
        version: 53,
        name: "forecast_attribution",
        compat: MigrationCompatibility::Additive,
        apply: admission_forecasts::apply_forecast_attribution,
    },
    Migration {
        version: 54,
        name: "retention_analytics",
        compat: MigrationCompatibility::Additive,
        apply: task_lock_denials::apply_retention_analytics,
    },
    // ORB-17304: each executor's latest declared capability per workspace,
    // which admission orders against. A new table only, which an older
    // binary never reads, so drains on older binaries keep writing.
    Migration {
        version: 55,
        name: "executor_census",
        compat: MigrationCompatibility::Additive,
        apply: executor_census::apply_executor_census,
    },
    // ORB-17288: a crew that sets `env` is its own provider account. A new
    // table for its usage readings and a nullable `invocations.account`
    // column; an older binary never reads the table and keeps inserting
    // invocations without the column, so drains on older binaries keep
    // writing.
    Migration {
        version: 56,
        name: "provider_crew_accounts",
        compat: MigrationCompatibility::Additive,
        apply: provider_account_limit_observations::apply_provider_crew_accounts,
    },
    // Restore older lifecycle readers without reinterpreting a handoff as
    // fail/release. Only attribution moves to a row family they never touch;
    // authority, receipts and bundle recovery intents keep their bytes.
    // A write guard also fences already-open pre-repair handoff writers;
    // that restriction makes the migration read-compatible, not additive.
    Migration {
        version: 57,
        name: "handoff_settlement_sidecars",
        compat: MigrationCompatibility::ReadCompatible,
        apply: task_coordination_rows::apply_handoff_settlement_sidecars,
    },
    // Crew performance phase 2: the persisted implementer by-type snapshot
    // a crew draw reads instead of scanning. An older binary never reads
    // or writes either table.
    Migration {
        version: 58,
        name: "crew_performance_snapshot",
        compat: MigrationCompatibility::Additive,
        apply: crew_performance_snapshot::apply_crew_performance_snapshot,
    },
    // Materialized task ownership, maintained by triggers even for older
    // writers. Seek each task's recency prefix instead of scanning JSON.
    Migration {
        version: 59,
        name: "job_run_task_recency",
        compat: MigrationCompatibility::Additive,
        apply: job_run_tasks::apply_job_run_task_recency,
    },
    // ORB-18383: lines added and deleted per landed delivery, split into
    // code, tests and other. A new table only, which an older binary never
    // reads or writes.
    Migration {
        version: 60,
        name: "landed_diff_stats",
        compat: MigrationCompatibility::Additive,
        apply: landed_diff_stats::apply_landed_diff_stats,
    },
    // Path leases: `task_reservations` rows held by a principal, advisory or
    // hard. Defaulted columns only; lease rows stay `scope = 'files'`, so an
    // older binary reads each one, advisory or not, as an exclusive file
    // reservation (fail safe), and the rows it writes read here as hard task
    // reservations.
    Migration {
        version: 61,
        name: "task_reservation_leases",
        compat: MigrationCompatibility::Additive,
        apply: task_reservations::apply_task_reservation_leases,
    },
    // ORB-18385: stored backlog forecasts, at most one per workspace per
    // hour, scored later against what their tasks did. A new table only,
    // which an older binary never reads or writes.
    Migration {
        version: 62,
        name: "backlog_forecasts",
        compat: MigrationCompatibility::Additive,
        apply: backlog_forecasts::apply_backlog_forecasts,
    },
    // Experiments slice 1: experiment definitions, their arms, an
    // append-only event log and per-task assignments. New tables only,
    // which an older binary never reads or writes.
    Migration {
        version: 63,
        name: "experiments",
        compat: MigrationCompatibility::Additive,
        apply: experiments::apply_experiments,
    },
    Migration {
        version: 64,
        name: "experiment_admissions",
        compat: MigrationCompatibility::Additive,
        apply: experiment_admissions::apply_experiment_admissions,
    },
    Migration {
        version: 65,
        name: "lease_notices",
        compat: MigrationCompatibility::Additive,
        apply: lease_notices::apply_lease_notices,
    },
    // Lease intake: the lease token a listener request authenticated with.
    // One nullable column; an older binary never reads it, and the leases it
    // writes read here as having no token.
    Migration {
        version: 66,
        name: "task_reservation_lease_token",
        compat: MigrationCompatibility::Additive,
        apply: task_reservations::apply_task_reservation_lease_token,
    },
    // Lease queue: a waiting request or expansion keeps its selectors in
    // `queued_files_json`, outside the held `files_json` every overlap reader
    // sees. Three nullable columns; an older binary never reads them, so a
    // queued lease holds nothing for it either.
    Migration {
        version: 67,
        name: "task_reservation_lease_queue",
        compat: MigrationCompatibility::Additive,
        apply: task_reservations::apply_task_reservation_lease_queue,
    },
    // Resource profiles by regime: the crew model and Orbit version each run
    // minute was sampled under. Two nullable columns; an older binary never
    // reads them, and the minutes it writes read here as having no regime.
    Migration {
        version: 68,
        name: "resource_minute_regime",
        compat: MigrationCompatibility::Additive,
        apply: run_resource_minutes::apply_resource_minute_regime,
    },
    // The sampler starts each run at minute zero, and retention leaves its
    // marker until the run's other minute rows have been removed. Older
    // retention code does not preserve that order, so old binaries may read
    // this schema but must not write it.
    Migration {
        version: 69,
        name: "run_resource_minutes_freshness_index",
        compat: MigrationCompatibility::ReadCompatible,
        apply: run_resource_minutes::apply_run_resource_minutes_freshness_index,
    },
    // Delete every admission-only row so each task is admitted once more,
    // including rows from enabled admissions that matched no population;
    // stored rows cannot distinguish those from disabled-era admissions.
    // Rows with an assignment or exclusion stay final. This migration changes
    // data only: older binaries read a missing row as a task not yet admitted.
    Migration {
        version: 70,
        name: "reopen_legacy_experiment_admissions",
        compat: MigrationCompatibility::Additive,
        apply: experiment_admissions::apply_reopen_legacy_experiment_admissions,
    },
    // Prompt-size pricing: the usage of each request whose prompt may reach a
    // tier. One nullable column; an older binary never reads it, and the
    // invocations it writes read here as having no per-request usage.
    Migration {
        version: 71,
        name: "invocation_long_prompt_requests",
        compat: MigrationCompatibility::Additive,
        apply: invocations::apply_invocation_long_prompt_requests,
    },
    // Independent optional ledger only. Older writers never touch these tables;
    // no task projection or trigger requires assistance-aware writers.
    // [ORB-22808] Decision assistance was removed; the tables stay inert.
    Migration {
        version: 72,
        name: "decision_assistance",
        compat: MigrationCompatibility::Additive,
        apply: assistance_work::apply_decision_assistance,
    },
    Migration {
        version: 73,
        name: "worker_resource_limits",
        compat: MigrationCompatibility::Additive,
        apply: worker_resource_limits::apply_worker_resource_limits,
    },
    // Script steps phase 1: the host-local operator grant for one
    // workspace script at one content digest. A new table only, which an
    // older binary never reads; its job loader refuses `target: script:`
    // and runs nothing.
    Migration {
        version: 74,
        name: "script_grants",
        compat: MigrationCompatibility::Additive,
        apply: script_grant::apply_script_grants,
    },
    // A counter that triggers bump whenever the delivery freshness probe may
    // change, so a resource-profile cache hit skips the probe while it holds.
    // The triggers fire on an older binary's writes too, and an older binary
    // never reads the table.
    Migration {
        version: 75,
        name: "delivery_runs_stamp",
        compat: MigrationCompatibility::Additive,
        apply: delivery_runs_stamp::apply_delivery_runs_stamp,
    },
    // The succeeded-candidate invocation read looked up activity steps by the
    // primary key and then fetched `agent_response_json` just to compare the
    // step window. Index-only, which an older binary neither names nor needs.
    Migration {
        version: 76,
        name: "job_run_step_activity_window_index",
        compat: MigrationCompatibility::Additive,
        apply: job_run_steps::apply_job_run_step_activity_window_index,
    },
    // Condition triggers phase 2: per-consumer condition state (arm states,
    // holds, fire records) and the audit of operator clear-stall actions.
    // New tables only, which an older binary never reads; its loaders refuse
    // condition definitions and leave these rows alone.
    Migration {
        version: 77,
        name: "condition_triggers",
        compat: MigrationCompatibility::Additive,
        apply: automation_condition_states::apply_condition_triggers,
    },
    Migration {
        version: 78,
        name: "agent_execution_identity",
        compat: MigrationCompatibility::Additive,
        apply: agent_executions::apply_agent_execution_identity,
    },
    // `orbit experiment delete`: experiment events stay append-only while
    // their experiment exists, so a deletion can remove them after the
    // experiment row. An older binary never deletes experiments, and reads a
    // deleted one as never defined.
    Migration {
        version: 79,
        name: "experiment_delete",
        compat: MigrationCompatibility::Additive,
        apply: experiment_events::apply_experiment_delete,
    },
    Migration {
        version: 80,
        name: "application_consent",
        compat: MigrationCompatibility::Additive,
        apply: application_consents::apply_application_consent,
    },
    // Index-only workspace/window reads avoid table fetches for reliability.
    Migration {
        version: 81,
        name: "invocation_reliability_window_index",
        compat: MigrationCompatibility::Additive,
        apply: invocations::apply_invocation_reliability_window_index,
    },
    // Optional provenance only; older writers leave it unknown and preserve existing values.
    Migration {
        version: 82,
        name: "job_run_build",
        compat: MigrationCompatibility::Additive,
        apply: job_runs::apply_job_run_build,
    },
    // Defaulted flag only: an older binary inserts without it and reads every
    // row as a complete tool trace.
    Migration {
        version: 83,
        name: "invocation_tool_calls_incomplete",
        compat: MigrationCompatibility::Additive,
        apply: invocations::apply_invocation_tool_calls_incomplete,
    },
    // Nullable counts only. Existing rows stay NULL (unavailable) and are
    // not recomputed. An older binary never reads or writes the columns.
    Migration {
        version: 84,
        name: "landed_diff_reviewable_lines",
        compat: MigrationCompatibility::Additive,
        apply: landed_diff_stats::apply_landed_diff_reviewable_lines,
    },
    // Fleet notices slice 1: the notice table and its post and close
    // history. New tables only, which an older binary never reads or writes.
    Migration {
        version: 85,
        name: "notices",
        compat: MigrationCompatibility::Additive,
        apply: notices::apply_notices,
    },
    // Separate, additive capability-gap ledger; older binaries leave it alone.
    Migration {
        version: 86,
        name: "evolution",
        compat: MigrationCompatibility::Additive,
        apply: evolution_records::apply_evolution,
    },
    // Crew memory slice M1: append-only memory revisions, promotion
    // proposals and their audit events. New tables only, which an older
    // binary never reads or writes.
    Migration {
        version: 87,
        name: "crew_memory",
        compat: MigrationCompatibility::Additive,
        apply: crew_memory_revisions::apply_crew_memory,
    },
    // Nullable MCP session model. Existing rows stay NULL. An older binary
    // never reads or writes the column, and it is not part of `role`.
    Migration {
        version: 88,
        name: "audit_session_model",
        compat: MigrationCompatibility::Additive,
        apply: audit_events::apply_audit_session_model,
    },
    // Partial indexes over unreleased task-reservation rows. Index-only:
    // older binaries keep reading and writing the table.
    Migration {
        version: 89,
        name: "task_reservation_unreleased_expires",
        compat: MigrationCompatibility::Additive,
        apply: task_reservations::apply_task_reservation_unreleased_expires_indexes,
    },
    // Partial expression indexes over claim and lifecycle payloads. Index-only:
    // older binaries keep reading and writing the table, and SQLite maintains
    // the indexes on their writes.
    Migration {
        version: 90,
        name: "task_coordination_claim_indexes",
        compat: MigrationCompatibility::Additive,
        apply: task_coordination_rows::apply_task_coordination_claim_indexes,
    },
    // ORB-24423: per-task agent scorecard facts kept between scans. A new
    // derived-cache table only, which an older binary never reads or writes.
    Migration {
        version: 91,
        name: "agent_task_facts",
        compat: MigrationCompatibility::Additive,
        apply: agent_task_facts::apply_agent_task_facts,
    },
    // Non-unique indexes preserve legacy workspace-scoped collisions while
    // new allocations reserve a host-wide identity in the shared transaction.
    Migration {
        version: 92,
        name: "job_run_host_id_indexes",
        compat: MigrationCompatibility::Additive,
        apply: job_runs::apply_job_run_host_id_indexes,
    },
    // ORB-25103: host-local routine circuit breakers and their transition
    // log. New tables only, which an older binary never reads or writes.
    Migration {
        version: 93,
        name: "routine_breakers",
        compat: MigrationCompatibility::Additive,
        apply: routine_breakers::apply_routine_breakers,
    },
    // Data backfill only: `crew_model` takes the legacy `implementer_model`
    // where it is unset. An older binary still reads either column.
    Migration {
        version: 94,
        name: "job_run_crew_model_backfill",
        compat: MigrationCompatibility::Additive,
        apply: job_runs::apply_job_run_crew_model_backfill,
    },
    // ORB-25965: the per-workspace drain intent (standing drain phase 1).
    // A new table only, which an older binary never reads or writes.
    Migration {
        version: 95,
        name: "drain_intents",
        compat: MigrationCompatibility::Additive,
        apply: drain_intents::apply_drain_intents,
    },
    // Per-task codebase analytics facts kept between scans. A new derived-cache
    // table only, which an older binary either ignores or already created
    // lazily with the same schema.
    Migration {
        version: 96,
        name: "codebase_task_facts",
        compat: MigrationCompatibility::Additive,
        apply: codebase_task_facts::apply_codebase_task_facts,
    },
];

/// Highest schema version this binary knows how to produce. Public for
/// the future `orbit migrate` surface (P3.4), alongside
/// [`AppliedMigration`] and the `Store` version accessors.
pub const SUPPORTED_SCHEMA_VERSION: u32 = 96;

/// Bring the store database up to the newest supported schema version,
/// applying any pending versioned migrations (each transactional and
/// recorded in the `schema_meta` ledger).
///
/// A database whose recorded schema version is newer than this binary
/// supports is decided by its forward-compatibility record (ORB-12434):
/// `Ok(Some(..))` means the caller must hold the connection read-only, and
/// an error means the database must not be opened at all.
pub(crate) fn apply_schema(conn: &Connection) -> Result<Option<ForwardCompatibleOpen>, OrbitError> {
    ledger::run_store_migrations(conn, MIGRATIONS, None)
}

pub(crate) fn apply_schema_at_path(
    conn: &Connection,
    path: &Path,
) -> Result<Option<ForwardCompatibleOpen>, OrbitError> {
    ledger::run_store_migrations(conn, MIGRATIONS, Some(path))
}

/// Registry metadata for one schema migration not yet recorded as applied,
/// as surfaced by `orbit migrate --dry-run` (ORB-10012).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSchemaMigration {
    pub version: u32,
    pub name: &'static str,
}

/// Read-only view of a store database's migration ledger: the recorded
/// schema version plus the registry migrations still pending against it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaLedgerStatus {
    /// Schema version recorded in the ledger (0 for a fresh/pre-ledger or
    /// nonexistent database).
    pub current_version: u32,
    /// Registry migrations newer than `current_version`, in apply order.
    /// Empty when the database is current or newer than this binary
    /// (compare against [`SUPPORTED_SCHEMA_VERSION`] to distinguish).
    pub pending: Vec<PendingSchemaMigration>,
    /// Set when the database is newer than this binary supports but only by
    /// additive or read-compatible migrations, so it can still be opened —
    /// read-only unless [`ForwardCompatibleOpen::writable`] (ORB-12434).
    /// `None` also covers a newer database this binary must refuse — the
    /// open path carries that refusal and its reason.
    pub forward_compatible: Option<ForwardCompatibleOpen>,
}

/// Inspect the migration ledger of the store database at `db_path` without
/// opening it for writing — and therefore without triggering the automatic
/// migrations that [`crate::Store::open`] applies. A missing database reads
/// as version 0 with every registry migration pending (opening it would
/// create and migrate it). Powers `orbit migrate --dry-run`.
pub fn read_schema_ledger_status(db_path: &Path) -> Result<SchemaLedgerStatus, OrbitError> {
    let (current_version, forward_compatible) = if db_path.exists() {
        let conn = Connection::open_with_flags(
            db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| {
            sqlite_error(
                StorageLayer::Store,
                &e,
                format!(
                    "cannot open store database '{}' read-only: {e}",
                    db_path.display()
                ),
            )
        })?;
        let current_version = current_schema_version(&conn)?;
        let forward_compatible = if current_version > SUPPORTED_SCHEMA_VERSION {
            orbit_common::storage::sqlite::ledger::read_compat_record(&conn)
                .and_then(|record| {
                    evaluate_newer_state(
                        StateComponent::StoreSchema,
                        current_version,
                        SUPPORTED_SCHEMA_VERSION,
                        record,
                        true,
                    )
                })
                .ok()
        } else {
            None
        };
        (current_version, forward_compatible)
    } else {
        (0, None)
    };

    Ok(SchemaLedgerStatus {
        current_version,
        pending: pending_schema_migrations_after(current_version),
        forward_compatible,
    })
}

/// This binary's schema ledger as upgrade admission compares it.
pub(crate) fn schema_compatibility() -> orbit_common::fs::generation::LedgerCompatibility {
    orbit_common::fs::generation::LedgerCompatibility::from_registry(
        MIGRATIONS
            .iter()
            .map(|m| (m.version, m.compat.is_write_safe(), !m.compat.is_breaking())),
    )
}

/// The frozen feature schemas as upgrade admission compares them: each
/// feature's newest version and its newest breaking migration.
pub(crate) fn feature_compatibility() -> Vec<(
    &'static str,
    u32,
    orbit_common::fs::generation::FeatureFloor,
)> {
    feature_schema_meta::feature_compatibility(feature_ledgers::FEATURE_REGISTRIES)
}

/// Registry migrations newer than `current_version`, in apply order. Lets
/// callers that already know the recorded version (e.g. via
/// [`crate::Store::schema_version`]) list what is pending without another
/// database open.
pub fn pending_schema_migrations_after(current_version: u32) -> Vec<PendingSchemaMigration> {
    MIGRATIONS
        .iter()
        .filter(|m| m.version > current_version)
        .map(|m| PendingSchemaMigration {
            version: m.version,
            name: m.name,
        })
        .collect()
}

#[cfg(test)]
mod tests;
