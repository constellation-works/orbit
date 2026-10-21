//! `InvocationStoreBackend` for the SQLite `Store`, delegating to the
//! inherent record and metrics methods.

impl crate::contracts::InvocationStoreBackend for crate::driver::sqlite::connection::Store {
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
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<crate::contracts::ProviderLedgerEntry>, orbit_common::OrbitError> {
        Self::list_provider_ledger_entries(self, provider_names, since)
    }

    fn list_activity_invocation_metrics(
        &self,
    ) -> Result<Vec<crate::contracts::ActivityInvocationMetrics>, orbit_common::OrbitError> {
        Self::list_activity_invocation_metrics(self)
    }

    fn get_task_invocation_metrics(
        &self,
        task_id: &str,
    ) -> Result<crate::contracts::TaskInvocationMetrics, orbit_common::OrbitError> {
        Self::get_task_invocation_metrics(self, task_id)
    }

    fn list_tool_invocation_metrics(
        &self,
    ) -> Result<Vec<crate::contracts::ToolInvocationMetrics>, orbit_common::OrbitError> {
        Self::list_tool_invocation_metrics(self)
    }
}
