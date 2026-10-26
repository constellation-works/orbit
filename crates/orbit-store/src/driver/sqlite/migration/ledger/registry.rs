use orbit_common::OrbitError;
use rusqlite::Connection;

use crate::contracts::{MigrationCompatibility, StateComponent};

/// One SQLite database file that evolves through its own migration registry.
pub struct LedgeredDatabase {
    /// Which versioned state its compatibility decisions are about.
    pub component: StateComponent,
    /// How diagnostics name the database ("store database").
    pub noun: &'static str,
    /// Run each migration with foreign-key enforcement off. SQLite ignores
    /// `PRAGMA foreign_keys` inside a transaction, so a body that rebuilds a
    /// parent table (create, copy, drop, rename) cannot turn it off itself,
    /// and with enforcement on the drop would cascade into the children.
    pub foreign_keys_off: bool,
    /// Written inside each migration's transaction after its ledger row and
    /// compatibility record, from the registry and the version just applied.
    pub stamp: Option<LedgerStamp>,
}

/// A database-specific marker a [`LedgeredDatabase`] keeps in step with its
/// ledger, such as the task registry's `user_version` fence.
pub type LedgerStamp = fn(&Connection, &[Migration], u32) -> Result<(), OrbitError>;

/// The store database (`orbit.db`) and its [`MIGRATIONS`].
pub(crate) const STORE_DATABASE: LedgeredDatabase = LedgeredDatabase {
    component: StateComponent::StoreSchema,
    noun: "store database",
    foreign_keys_off: false,
    stamp: None,
};

/// One entry in the migration registry.
pub struct Migration {
    /// Positive schema version, strictly increasing within the registry.
    pub version: u32,
    /// Stable name recorded beside this version in the ledger.
    pub name: &'static str,
    /// What this migration means for a binary that does not have it. See
    /// [`MigrationCompatibility`]; declare `Breaking` when in doubt.
    pub compat: MigrationCompatibility,
    /// Frozen migration body, run inside the engine's transaction.
    pub apply: fn(&Connection) -> Result<(), OrbitError>,
}

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
        apply: super::super::apply_baseline_schema,
    },
    Migration {
        version: 2,
        name: "learnings_index_workspace_scope",
        // Drops and reshapes `learnings_index`, which binaries without it query.
        compat: MigrationCompatibility::Breaking,
        apply: super::super::apply_learning_index_workspace_scope,
    },
    Migration {
        version: 3,
        name: "flat_crew_model",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_flat_crew_model,
    },
    Migration {
        version: 4,
        name: "job_run_archive_stage",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_job_run_archive_stage,
    },
    Migration {
        version: 5,
        name: "host_registry_core",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_host_registry_core,
    },
    Migration {
        version: 6,
        name: "workspace_coordination_projections",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_workspace_coordination_projections,
    },
    Migration {
        version: 7,
        name: "trusted_mcp_audit_provenance",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_trusted_mcp_audit_provenance,
    },
    Migration {
        version: 8,
        name: "hub_registry_metadata",
        // Every host-registry write must bump `registry_revision` in the same
        // transaction; an older writer of those tables leaves it stale.
        compat: MigrationCompatibility::ReadCompatible,
        apply: super::super::apply_hub_registry_metadata,
    },
    Migration {
        version: 9,
        name: "feature_schema_ledger",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_feature_schema_ledger,
    },
    // ORB-10367: carries the invocation telemetry columns
    // (`cache_create_1h_tokens`, `provider_cost_usd`) to databases that
    // recorded the v1 baseline before those columns were added to it.
    Migration {
        version: 10,
        name: "invocation_telemetry_columns",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_invocation_telemetry_columns,
    },
    // ADR-0287: baseline v1 is frozen; schema added later always advances
    // through an append-only ledger entry.
    Migration {
        version: 11,
        name: "routine_scheduler_schema",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_routine_scheduler_schema,
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
        apply: super::super::apply_friction_records_schema,
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
        apply: super::super::apply_workspace_claim_scope,
    },
    // ORB-10736: keep the shipped learning migrations above intact, then
    // retire their projections explicitly for both upgraded and fresh stores.
    Migration {
        version: 14,
        name: "remove_native_learning_subsystem",
        // Drops the learning tables older binaries still query.
        compat: MigrationCompatibility::Breaking,
        apply: super::super::apply_remove_native_learning_subsystem,
    },
    Migration {
        version: 15,
        name: "invocation_audit_context",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_invocation_audit_context,
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
        apply: super::super::apply_audit_actor_identity,
    },
    // ORB-10890: the untrusted half of attribution. An MCP client started from
    // its own config cannot satisfy the managed-run trust boundary, so its
    // self-declared identity lands beside `role` rather than in it.
    Migration {
        version: 17,
        name: "audit_self_reported_actor",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_audit_self_reported_actor,
    },
    // Alias map v2: `fable` became a family rule so versioned Fable labels
    // resolve to `claude`. Rows stamped with the old map are re-derived.
    Migration {
        version: 18,
        name: "audit_actor_alias_v2",
        // An older writer keeps stamping rows with the v1 alias map, and
        // nothing re-derives them afterwards.
        compat: MigrationCompatibility::ReadCompatible,
        apply: super::super::apply_audit_actor_alias_v2,
    },
    // The run listing orders by `created_at DESC, run_id` per workspace; the
    // existing indexes cover `(workspace_id, job_id, scheduled_at)` and
    // `(workspace_id, state)`, so every dashboard page scanned and sorted a
    // workspace's whole run history.
    Migration {
        version: 19,
        name: "job_runs_created_index",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_job_runs_created_index,
    },
    // Every window filter (`ts >= ? AND ts < ?`) and the newest-first
    // listing (`ORDER BY ts DESC, id DESC LIMIT n`) over `invocations` had
    // only the job-run and activity indexes to work with, so each was a
    // full scan plus a temp sort.
    Migration {
        version: 20,
        name: "invocations_ts_index",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_invocations_ts_index,
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
        apply: super::super::apply_task_commit_journal,
    },
    Migration {
        version: 22,
        name: "execution_provenance",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_execution_provenance,
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
        apply: super::super::apply_audit_machine_name_columns,
    },
    // Plugin standard phase 1: the `plugins` table beside `tools`, plus the
    // audit columns carrying plugin name, version and manifest digest.
    Migration {
        version: 24,
        name: "plugins_and_audit_plugin_provenance",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_plugins_and_audit_plugin_provenance,
    },
    // Plugin standard phase 2: the grant set a plugin call ran under.
    Migration {
        version: 25,
        name: "audit_plugin_grants",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_audit_plugin_grants,
    },
    // Operation mode was removed on 2026-09-21 (ORB-12772): the grant and
    // recovery-ledger tables its `operation` feature migration created go
    // with it. Breaking: an older binary reads `operation_grants` at handoff
    // commit and would fail there rather than silently skipping the check.
    Migration {
        version: 26,
        name: "remove_operation_mode",
        compat: MigrationCompatibility::Breaking,
        apply: super::super::apply_remove_operation_mode,
    },
    // Plugin standard phase 4: the Orbit version a plugin's conformance
    // goldens last passed on (`orbit plugin test`).
    Migration {
        version: 27,
        name: "plugin_certified_orbit_version",
        // A reinstall must clear the certification when the manifest changes;
        // an older upsert keeps it for bytes that were never tested.
        compat: MigrationCompatibility::ReadCompatible,
        apply: super::super::apply_plugin_certified_orbit_version,
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
        apply: super::super::apply_plugin_archive_digest,
    },
    // Friction re-homing: the workspace that owns a friction recorded in
    // another one, so curation can finish with a durable disposition.
    Migration {
        version: 29,
        name: "friction_rehome_target",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_friction_rehome_target,
    },
    // Plugin secrets: the names of the declared secrets a plugin call
    // delivered, never their values.
    Migration {
        version: 30,
        name: "audit_plugin_secrets",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_audit_plugin_secrets,
    },
    // Plugin secret rotation: each secret a backend asked to rotate, by
    // name, and whether the update was applied or refused — never a value.
    Migration {
        version: 31,
        name: "audit_plugin_secret_updates",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_audit_plugin_secret_updates,
    },
    // The plugin broker: whether a call was brokered for a sandboxed agent,
    // and the peer PID that asked.
    Migration {
        version: 32,
        name: "audit_brokered_call",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_audit_brokered_call,
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
        apply: super::super::apply_job_run_id_allocations,
    },
    // Per-job newest-first reads sorted every run the job ever had, and the
    // retry children read scanned the workspace for want of an index that
    // orders by `created_at`. Both are pure `CREATE INDEX IF NOT EXISTS`,
    // which an older binary ignores.
    Migration {
        version: 34,
        name: "job_runs_job_created_and_retry_indexes",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_job_runs_job_created_and_retry_indexes,
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
        apply: super::super::apply_plugin_build_record,
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
        apply: super::super::apply_invocation_workspace_scope,
    },
    Migration {
        version: 37,
        name: "audit_tool_call_index",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_audit_tool_call_index,
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
        apply: super::super::apply_job_run_states,
    },
    // Recency-ordered run pages (the dashboard's aggregate run list) sorted
    // every run of the workspace for want of an index over the recency
    // expression. Pure `CREATE INDEX IF NOT EXISTS`, which an older binary
    // ignores and SQLite maintains on its writes.
    Migration {
        version: 39,
        name: "job_runs_recency_index",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_job_runs_recency_index,
    },
    // ORB-14695: the host's provider usage limits. A new table only, which an
    // older binary never reads, so drains on older binaries keep writing.
    Migration {
        version: 40,
        name: "provider_limit_observations",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_provider_limit_observations,
    },
    // ORB-14696: a provider's own usage-window reading on the same rows. New
    // nullable or defaulted columns only, so older binaries keep writing.
    Migration {
        version: 41,
        name: "provider_limit_readings",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_provider_limit_readings,
    },
    // ORB-14699: the provider an invocation ran on, beside its agent. One new
    // nullable column, so older binaries keep inserting.
    Migration {
        version: 42,
        name: "invocation_provider",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_invocation_provider,
    },
    // Windowed reads of one v2 event type (the audit summary's policy
    // denials) had no index over both the type and the window. Index-only,
    // which an older binary neither names nor needs.
    Migration {
        version: 43,
        name: "v2_audit_event_type_ts_index",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_v2_audit_event_type_ts_index,
    },
    // Per-minute CPU and memory of each delivery run's worker scope, for
    // auto-concurrency profiles. A new table only, which an older binary never
    // names, so drains on older binaries keep writing.
    Migration {
        version: 44,
        name: "run_resource_minutes",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_run_resource_minutes,
    },
    // The host's CPU and memory per wall-clock minute, the ground truth for
    // the forecast. A new table only, as v44.
    Migration {
        version: 45,
        name: "host_resource_minutes",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_host_resource_minutes,
    },
    Migration {
        version: 46,
        name: "job_run_step_lock_wait",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_job_run_step_lock_wait,
    },
    // Admissions decided under the host admission lock whose runs may not
    // exist yet, so every drain's forecast counts them. A new table only, as
    // v44.
    Migration {
        version: 47,
        name: "admission_reservations",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_admission_reservations,
    },
    // Each drain admission pass's forecast and decisions, checked against
    // `host_resource_minutes` by the forecast accuracy report. A new table
    // only, as v44.
    Migration {
        version: 48,
        name: "admission_forecasts",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_admission_forecasts,
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
        apply: super::super::apply_absorb_feature_ledgers,
    },
    Migration {
        version: 50,
        name: "pull_forecast_decisions",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_pull_forecast_decisions,
    },
    Migration {
        version: 51,
        name: "worker_cpu_accounting",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_worker_cpu_accounting,
    },
    // ORB-16379: the crews this host's provider was unavailable for. A new
    // table only, which an older binary never reads, so drains on older
    // binaries keep writing.
    Migration {
        version: 52,
        name: "provider_crew_outages",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_provider_crew_outages,
    },
    Migration {
        version: 53,
        name: "forecast_attribution",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_forecast_attribution,
    },
    Migration {
        version: 54,
        name: "retention_analytics",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_retention_analytics,
    },
    // ORB-17304: each executor's latest declared capability per workspace,
    // which admission orders against. A new table only, which an older
    // binary never reads, so drains on older binaries keep writing.
    Migration {
        version: 55,
        name: "executor_census",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_executor_census,
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
        apply: super::super::apply_provider_crew_accounts,
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
        apply: super::super::apply_handoff_settlement_sidecars,
    },
    // Crew performance phase 2: the persisted implementer by-type snapshot
    // a crew draw reads instead of scanning. An older binary never reads
    // or writes either table.
    Migration {
        version: 58,
        name: "crew_performance_snapshot",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_crew_performance_snapshot,
    },
    // Materialized task ownership, maintained by triggers even for older
    // writers. Seek each task's recency prefix instead of scanning JSON.
    Migration {
        version: 59,
        name: "job_run_task_recency",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_job_run_task_recency,
    },
    // ORB-18383: lines added and deleted per landed delivery, split into
    // code, tests and other. A new table only, which an older binary never
    // reads or writes.
    Migration {
        version: 60,
        name: "landed_diff_stats",
        compat: MigrationCompatibility::Additive,
        apply: super::super::apply_landed_diff_stats,
    },
];

/// Highest schema version this binary knows how to produce. Public for
/// the future `orbit migrate` surface (P3.4), alongside
/// [`AppliedMigration`] and the `Store` version accessors.
pub const SUPPORTED_SCHEMA_VERSION: u32 = 60;

/// A migration recorded as applied in the `schema_meta` ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMigration {
    pub version: u32,
    pub name: String,
    pub applied_at: String,
}
