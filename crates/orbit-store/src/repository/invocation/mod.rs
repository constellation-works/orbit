//! Concrete repository for invocation traces (`invocations`,
//! `invocation_tasks`, `tool_calls`) and agent dispatch records
//! (`agent_executions`).
//!
//! Reached only through [`Snapshot::invocation`] (read-only) and
//! [`Tx::invocation`] (read-write inside [`crate::Store::write`]).

mod executions;
mod metrics;
mod records;

use std::collections::HashMap;
use std::marker::PhantomData;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::telemetry::AgentExecutionIdentity;
use rusqlite::Connection;

use crate::contract::{
    AgentExecutionBackfill, AgentExecutionQuery, AgentExecutionRecord, InvocationAccountingFact,
    InvocationAccountingQuery, InvocationInsertParams, InvocationQuery, InvocationRecord,
    InvocationSample, ProviderLedgerEntry, TaskInvocationMetrics, ToolInvocationMetrics,
};
use crate::store::{Snapshot, Tx};

pub(crate) use executions::{
    AgentExecutionSlotRow, UnidentifiedInvocationRow, UnidentifiedInvocationTarget,
};

/// Concrete invocation and agent-execution repository scoped to a [`Snapshot`]
/// (read-only) or a [`Tx`] (read-write).
pub struct InvocationRepository<'a, Access> {
    conn: &'a Connection,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> InvocationRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            _access: PhantomData,
        }
    }
}

impl<'a, 'tx> InvocationRepository<'a, Tx<'tx>> {
    pub(crate) fn for_tx(tx: &'a Tx<'tx>) -> Self {
        Self {
            conn: tx.connection(),
            _access: PhantomData,
        }
    }

    /// Persist immutable launch identity before a provider starts.
    pub fn insert_agent_execution(
        &self,
        workspace: &str,
        run_id: &str,
        activity_id: &str,
        provider: &str,
        model: Option<&str>,
        identity: &AgentExecutionIdentity,
    ) -> Result<i64, OrbitError> {
        executions::insert_agent_execution(
            self.conn,
            workspace,
            run_id,
            activity_id,
            provider,
            model,
            identity,
        )
    }

    /// Persist one backfilled launch identity with its provenance and launch timestamp.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn insert_agent_execution_with_backfill(
        &self,
        workspace: &str,
        run_id: &str,
        activity_id: &str,
        provider: &str,
        model: Option<&str>,
        started_at: DateTime<Utc>,
        identity: &AgentExecutionIdentity,
        backfill: &AgentExecutionBackfill,
    ) -> Result<i64, OrbitError> {
        executions::insert_agent_execution_with_backfill(
            self.conn,
            workspace,
            run_id,
            activity_id,
            provider,
            model,
            started_at,
            identity,
            backfill,
        )
    }

    /// Link an unidentified invocation row to its backfilled `agent_executions` row.
    pub(crate) fn link_agent_execution(
        &self,
        invocation_id: i64,
        execution_id: i64,
    ) -> Result<(), OrbitError> {
        executions::link_agent_execution(self.conn, invocation_id, execution_id)
    }

    /// Insert one invocation trace, its task links and its tool calls.
    pub fn insert(
        &self,
        workspace_id: &str,
        params: &InvocationInsertParams,
    ) -> Result<(), OrbitError> {
        records::insert(self.conn, workspace_id, params)
    }
}

impl<Access> InvocationRepository<'_, Access> {
    /// Launch identities of one workspace-qualified run in launch order.
    pub fn agent_executions(
        &self,
        workspace: &str,
        run_id: &str,
    ) -> Result<Vec<AgentExecutionRecord>, OrbitError> {
        executions::agent_executions(self.conn, workspace, run_id)
    }

    /// Launch identities in a workspace and date window, in launch order.
    pub fn list_agent_executions(
        &self,
        query: &AgentExecutionQuery,
    ) -> Result<Vec<AgentExecutionRecord>, OrbitError> {
        executions::list_agent_executions(self.conn, query)
    }

    /// Invocations matching `filter`, hydrated with task ids and (unless skipped) tool calls.
    pub fn list(&self, filter: &InvocationQuery) -> Result<Vec<InvocationRecord>, OrbitError> {
        records::list(self.conn, filter)
    }

    /// Every invocation in the requested workspace and half-open window, hydrated
    /// with distinct linked task ids.
    pub fn list_accounting_facts(
        &self,
        query: &InvocationAccountingQuery,
    ) -> Result<Vec<InvocationAccountingFact>, OrbitError> {
        records::list_accounting_facts(self.conn, query)
    }

    /// Every invocation of the provider account at or after `since`, oldest first,
    /// across every workspace on this host.
    pub fn list_provider_ledger_entries(
        &self,
        provider_names: &[String],
        account: Option<&str>,
        since: DateTime<Utc>,
    ) -> Result<Vec<ProviderLedgerEntry>, OrbitError> {
        records::list_provider_ledger_entries(self.conn, provider_names, account, since)
    }

    /// Every invocation with a resolved model in one workspace partition, in storage order.
    pub fn list_model_attributed_samples(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<InvocationSample>, OrbitError> {
        metrics::list_model_attributed_samples(self.conn, workspace_id)
    }

    /// Aggregated invocation metrics for one task.
    pub fn task_metrics(&self, task_id: &str) -> Result<TaskInvocationMetrics, OrbitError> {
        metrics::task_metrics(self.conn, task_id)
    }

    /// Tool aggregates for one workspace partition, all time.
    pub fn tool_metrics(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<ToolInvocationMetrics>, OrbitError> {
        metrics::tool_metrics(self.conn, workspace_id)
    }

    /// Completed invocations of `workspace_id` without a dispatch identity, ordered by `(job_run_id, id)`.
    pub(crate) fn list_unidentified(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<UnidentifiedInvocationRow>, OrbitError> {
        executions::list_unidentified(self.conn, workspace_id)
    }

    /// Read one invocation's dispatch slot only if it still lacks an `agent_execution_id`.
    pub(crate) fn unidentified_target(
        &self,
        workspace_id: &str,
        invocation_id: i64,
    ) -> Result<Option<UnidentifiedInvocationTarget>, OrbitError> {
        executions::unidentified_target(self.conn, workspace_id, invocation_id)
    }

    /// Map of `run_id -> resolved_crew` for every job run in `workspace_id`.
    pub(crate) fn list_job_run_crews(
        &self,
        workspace_id: &str,
    ) -> Result<HashMap<String, Option<String>>, OrbitError> {
        executions::list_job_run_crews(self.conn, workspace_id)
    }

    /// Existing `agent_executions` rows for one `(workspace_id, run_id, activity_id, provider, model)` slot.
    pub(crate) fn agent_executions_for_slot(
        &self,
        workspace_id: &str,
        run_id: &str,
        activity_id: &str,
        provider: &str,
        model: Option<&str>,
    ) -> Result<Vec<AgentExecutionSlotRow>, OrbitError> {
        executions::agent_executions_for_slot(
            self.conn,
            workspace_id,
            run_id,
            activity_id,
            provider,
            model,
        )
    }
}
