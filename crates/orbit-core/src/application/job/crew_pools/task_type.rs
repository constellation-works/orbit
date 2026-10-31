//! Type crew pools: per `workflow.crew_performance_types` type and complexity
//! tier, a weighted pool (`workflow.type_complexity_crews.<type>.<tier>`)
//! that replaces that tier's complexity pool for a task of the type.
//!
//! A task's type is [`orbit_config::classify_task_type`] over its current
//! tags and context files, the lens the implementer scorecard measures by.
//! Type pools draw behind an explicit crew and a purpose pool and ahead of
//! the complexity pool; `other` never matches a pool. A pool whose members
//! are all disabled or excluded by executor OS keeps the complexity route.

use orbit_common::OrbitError;
use orbit_config::{OTHER_TYPE, canonical_crew_pool, type_complexity_crews_key};
use orbit_types::task::{Task, TaskComplexity, TaskOsRequirement};

use crate::OrbitRuntime;
use crate::application::contracts::{CrewCandidate, parse_type_crew_source, type_crew_source};

/// The type pool a task routes to: its classified type and its tier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypePoolRoute {
    pub(crate) task_type: String,
    pub(crate) tier: TaskComplexity,
}

impl TypePoolRoute {
    /// The `type:<type>/<tier>` provenance a draw from this pool records.
    pub(crate) fn crew_source(&self) -> String {
        type_crew_source(&self.task_type, self.tier)
    }
}

impl OrbitRuntime {
    /// The type pool a task with `tags` and `context_files` at `complexity`
    /// draws from, or `None` when it is unassessed, classifies as `other`, or
    /// its type has no pool for that tier.
    pub(crate) fn task_type_pool_route(
        &self,
        complexity: Option<TaskComplexity>,
        tags: &[String],
        context_files: &[String],
    ) -> Option<TypePoolRoute> {
        let tier = complexity.filter(|complexity| complexity.is_assessed())?;
        let pools = self.context.settings().type_complexity_crews();
        // Classification compiles path globs, so a workspace without type
        // pools skips it.
        pools.iter().next()?;
        let task_type =
            orbit_config::classify_task_type(self.crew_performance_types(), tags, context_files);
        if task_type == OTHER_TYPE {
            return None;
        }
        pools.pool(task_type, tier)?;
        Some(TypePoolRoute {
            task_type: task_type.to_string(),
            tier,
        })
    }

    /// The enabled members of `route`'s pool this host may draw, with the
    /// `workflow.type_complexity_crews.<type>.<tier>` key they come from.
    /// `task_os` selects the creation filter, as for the complexity pools.
    /// `None` when the pool is unset, or wholly disabled or excluded.
    pub(crate) fn type_pool_candidates(
        &self,
        route: &TypePoolRoute,
        task_os: Option<&TaskOsRequirement>,
    ) -> Result<Option<(Vec<CrewCandidate>, String)>, OrbitError> {
        let settings = self.context.settings();
        let Some(raw) = settings
            .type_complexity_crews()
            .pool(&route.task_type, route.tier)
        else {
            return Ok(None);
        };
        let key = type_complexity_crews_key(&route.task_type, route.tier);
        let pool = canonical_crew_pool(raw, settings.crews(), &key)?;
        self.pool_member_candidates(&pool.entries, &key, task_os)
    }

    /// The type pool `task` was assigned from, when its source is
    /// `type:<type>/<tier>` and that pool still has drawable members. A hold
    /// or a usage limit retries this pool before the complexity pool.
    pub(crate) fn type_assignment_candidates(
        &self,
        task: &Task,
    ) -> Result<Option<(Vec<CrewCandidate>, String)>, OrbitError> {
        let Some(source) = self.task_crew_source(task)? else {
            return Ok(None);
        };
        let Some((task_type, tier)) = parse_type_crew_source(&source) else {
            return Ok(None);
        };
        self.type_pool_candidates(
            &TypePoolRoute {
                task_type: task_type.to_string(),
                tier,
            },
            None,
        )
    }
}
