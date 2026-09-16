mod metrics;
mod records;

/// [ORB-10367] Insert-bound invocation columns, re-exported for the schema
/// drift regression test in `sqlite::migration::tests`.
#[cfg(test)]
pub(crate) use records::INVOCATION_INSERT_COLUMNS;

impl crate::contracts::InvocationStoreBackend for crate::driver::sqlite::connection::Store {
    fn insert_invocation_trace_record(
        &self,
        params: &crate::contracts::InvocationInsertParams,
    ) -> Result<(), orbit_common::OrbitError> {
        Self::insert_invocation_trace_record(self, params)
    }

    fn list_invocation_records(
        &self,
        filter: &crate::contracts::InvocationQuery,
    ) -> Result<Vec<crate::contracts::InvocationRecord>, orbit_common::OrbitError> {
        Self::list_invocation_records(self, filter)
    }

    fn list_invocation_accounting_facts(
        &self,
        query: &crate::contracts::InvocationAccountingQuery,
    ) -> Result<Vec<crate::contracts::InvocationAccountingFact>, orbit_common::OrbitError> {
        Self::list_invocation_accounting_facts(self, query)
    }

    fn list_activity_invocation_metrics(
        &self,
    ) -> Result<Vec<crate::contracts::ActivityInvocationMetrics>, orbit_common::OrbitError> {
        Self::list_activity_invocation_metrics(self)
    }

    fn list_agent_invocation_metrics(
        &self,
    ) -> Result<Vec<crate::contracts::AgentInvocationMetrics>, orbit_common::OrbitError> {
        Self::list_agent_invocation_metrics(self)
    }

    fn get_task_invocation_metrics(
        &self,
        task_id: &str,
    ) -> Result<crate::contracts::TaskInvocationMetrics, orbit_common::OrbitError> {
        Self::get_task_invocation_metrics(self, task_id)
    }

    fn list_top_task_invocation_metrics(
        &self,
        limit: usize,
    ) -> Result<Vec<crate::contracts::TaskInvocationMetrics>, orbit_common::OrbitError> {
        Self::list_top_task_invocation_metrics(self, limit)
    }

    fn list_tool_invocation_metrics(
        &self,
    ) -> Result<Vec<crate::contracts::ToolInvocationMetrics>, orbit_common::OrbitError> {
        Self::list_tool_invocation_metrics(self)
    }

    fn invocation_scoreboard_watermark(&self) -> Result<Option<u64>, orbit_common::OrbitError> {
        Self::invocation_scoreboard_watermark(self)
    }
}
