use orbit_common::OrbitError;

pub use orbit_search::{
    CompanionStatus, IndexKind, ScoreBreakdown, SemanticHit, SemanticIndexParams,
    SemanticIndexResult, SemanticInstallParams, SemanticInstallResult, SemanticRelatedParams,
    SemanticRelatedResult, SemanticSearchParams, SemanticSearchResult, SemanticStatsResult,
    SemanticUninstallParams, SemanticUninstallResult, TaskIndexResult,
};

use crate::OrbitRuntime;
use crate::application::task::TaskListFilter;

impl OrbitRuntime {
    pub fn semantic_install(
        &self,
        params: SemanticInstallParams,
    ) -> Result<SemanticInstallResult, OrbitError> {
        let result = orbit_search::semantic_install(params)?;
        // A cached companion is a running copy of the binary that was just
        // replaced; forget it so the next request starts the installed one.
        self.stores().semantic_embedders().clear();
        Ok(result)
    }

    pub fn semantic_uninstall(
        &self,
        params: SemanticUninstallParams,
    ) -> Result<SemanticUninstallResult, OrbitError> {
        let result = orbit_search::semantic_uninstall(params)?;
        self.stores().semantic_embedders().clear();
        Ok(result)
    }

    pub fn semantic_index(
        &self,
        params: SemanticIndexParams,
    ) -> Result<SemanticIndexResult, OrbitError> {
        match params.resolved_kind() {
            IndexKind::Tasks | IndexKind::All => self
                .semantic_index_tasks(params)
                .map(SemanticIndexResult::from),
        }
    }

    fn semantic_index_tasks(
        &self,
        params: SemanticIndexParams,
    ) -> Result<TaskIndexResult, OrbitError> {
        let tasks = self.stores().tasks().list_tasks()?;
        orbit_search::semantic_index(
            self.stores().semantic_index().store()?,
            &tasks,
            self.stores().semantic_embedders(),
            params,
        )
    }

    pub fn semantic_stats(&self) -> Result<SemanticStatsResult, OrbitError> {
        let task_ids: Vec<String> = self
            .task_candidates(&TaskListFilter::default(), usize::MAX)?
            .items
            .into_iter()
            .map(|task| task.id)
            .collect();
        orbit_search::semantic_stats(self.stores().semantic_index().store()?, &task_ids)
    }

    pub fn semantic_search(
        &self,
        params: SemanticSearchParams,
    ) -> Result<SemanticSearchResult, OrbitError> {
        orbit_search::semantic_search(
            self.stores().semantic_index().store()?,
            self.stores().semantic_embedders(),
            params,
        )
    }

    pub fn semantic_related(
        &self,
        params: SemanticRelatedParams,
    ) -> Result<SemanticRelatedResult, OrbitError> {
        let target = self.get_task(&params.task_id)?;
        orbit_search::semantic_related(
            self.stores().semantic_index().store()?,
            &target,
            self.stores().semantic_embedders(),
            params,
        )
    }
}
