//! `InvocationStoreBackend` for the SQLite `Store`, delegating to the
//! inherent record and metrics methods.

impl crate::contracts::InvocationStoreBackend for crate::driver::sqlite::connection::Store {
    fn begin_agent_execution(
        &self,
        workspace: &str,
        run_id: &str,
        activity_id: &str,
        provider: &str,
        model: Option<&str>,
        identity: &orbit_types::telemetry::AgentExecutionIdentity,
    ) -> Result<i64, orbit_common::OrbitError> {
        Self::begin_agent_execution(
            self,
            workspace,
            run_id,
            activity_id,
            provider,
            model,
            identity,
        )
    }
    fn agent_executions(
        &self,
        workspace: &str,
        run_id: &str,
    ) -> Result<Vec<crate::contracts::AgentExecutionRecord>, orbit_common::OrbitError> {
        Self::agent_executions(self, workspace, run_id)
    }

    fn insert_invocation_trace_record(
        &self,
        workspace_id: &str,
        params: &crate::contracts::InvocationInsertParams,
    ) -> Result<(), orbit_common::OrbitError> {
        Self::insert_invocation_trace_record(self, workspace_id, params)
    }

    fn list_invocation_records(
        &self,
        filter: &crate::contracts::InvocationQuery,
    ) -> Result<Vec<crate::contracts::InvocationRecord>, orbit_common::OrbitError> {
        Self::list_invocation_records(self, filter)
    }

    fn latest_activity_models(
        &self,
        workspace_id: &str,
        activity_id: &str,
        run_ids: &[String],
    ) -> Result<std::collections::HashMap<String, Option<String>>, orbit_common::OrbitError> {
        Self::latest_activity_models(self, workspace_id, activity_id, run_ids)
    }

    fn list_invocation_accounting_facts(
        &self,
        query: &crate::contracts::InvocationAccountingQuery,
    ) -> Result<Vec<crate::contracts::InvocationAccountingFact>, orbit_common::OrbitError> {
        Self::list_invocation_accounting_facts(self, query)
    }

    fn list_provider_ledger_entries(
        &self,
        provider_names: &[String],
        account: Option<&str>,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<crate::contracts::ProviderLedgerEntry>, orbit_common::OrbitError> {
        Self::list_provider_ledger_entries(self, provider_names, account, since)
    }

    fn list_activity_invocation_metrics(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<crate::contracts::ActivityInvocationMetrics>, orbit_common::OrbitError> {
        Self::list_activity_invocation_metrics(self, workspace_id)
    }

    fn get_task_invocation_metrics(
        &self,
        task_id: &str,
    ) -> Result<crate::contracts::TaskInvocationMetrics, orbit_common::OrbitError> {
        Self::get_task_invocation_metrics(self, task_id)
    }

    fn list_tool_invocation_metrics(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<crate::contracts::ToolInvocationMetrics>, orbit_common::OrbitError> {
        Self::list_tool_invocation_metrics(self, workspace_id)
    }
}
