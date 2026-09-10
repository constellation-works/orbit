use orbit_common::OrbitError;

pub use orbit_search::{
    CompanionStatus, IndexKind, ScoreBreakdown, SemanticHit, SemanticIndexParams,
    SemanticIndexResult, SemanticInstallParams, SemanticInstallResult, SemanticRelatedParams,
    SemanticRelatedResult, SemanticSearchParams, SemanticSearchResult, SemanticStatsResult,
    SemanticUninstallParams, SemanticUninstallResult, TaskIndexResult,
};

use crate::OrbitRuntime;

impl OrbitRuntime {
    pub fn semantic_install(
        &self,
        params: SemanticInstallParams,
    ) -> Result<SemanticInstallResult, OrbitError> {
        orbit_search::semantic_install(params)
    }

    pub fn semantic_uninstall(
        &self,
        params: SemanticUninstallParams,
    ) -> Result<SemanticUninstallResult, OrbitError> {
        orbit_search::semantic_uninstall(params)
    }

    pub fn semantic_index(
        &self,
        params: SemanticIndexParams,
    ) -> Result<SemanticIndexResult, OrbitError> {
        match params.resolved_kind() {
            IndexKind::Tasks => self
                .semantic_index_tasks(params)
                .map(SemanticIndexResult::from),
            IndexKind::Docs => self
                .semantic_index_docs(params)
                .map(SemanticIndexResult::from),
            IndexKind::All => {
                let tasks = self.semantic_index_tasks(params.clone());
                let docs = self.semantic_index_docs(params);
                match (tasks, docs) {
                    (Ok(tasks), Ok(docs)) => Ok(SemanticIndexResult::All { tasks, docs }),
                    (Err(error), _) => Err(error),
                    (_, Err(error)) => Err(error),
                }
            }
        }
    }

    fn semantic_index_tasks(
        &self,
        params: SemanticIndexParams,
    ) -> Result<TaskIndexResult, OrbitError> {
        let tasks = self.stores().tasks().list_tasks()?;
        orbit_search::semantic_index(self.stores().semantic_index().store()?, &tasks, params)
    }

    fn semantic_index_docs(
        &self,
        params: SemanticIndexParams,
    ) -> Result<orbit_search::DocIndexResult, OrbitError> {
        self.index_docs(orbit_search::DocIndexParams {
            model: params.model,
            force: params.force,
        })
    }

    pub fn semantic_stats(&self) -> Result<SemanticStatsResult, OrbitError> {
        let task_ids: Vec<String> = self
            .stores()
            .tasks()
            .list_tasks()?
            .into_iter()
            .map(|task| task.id)
            .collect();
        orbit_search::semantic_stats(self.stores().semantic_index().store()?, &task_ids)
    }

    pub fn semantic_search(
        &self,
        params: SemanticSearchParams,
    ) -> Result<SemanticSearchResult, OrbitError> {
        orbit_search::semantic_search(self.stores().semantic_index().store()?, params)
    }

    pub fn semantic_related(
        &self,
        params: SemanticRelatedParams,
    ) -> Result<SemanticRelatedResult, OrbitError> {
        let tasks = self.stores().tasks().list_tasks()?;
        orbit_search::semantic_related(self.stores().semantic_index().store()?, &tasks, params)
    }
}
