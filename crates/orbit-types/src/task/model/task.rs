use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Task {
    pub id: OrbitId,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Exact canonical tool names the task adds to an agent activity baseline.
    #[serde(
        default,
        deserialize_with = "deserialize_required_tools",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub required_tools: Vec<String>,
    #[serde(default, alias = "instructions")]
    pub plan: String,
    #[serde(default)]
    pub execution_summary: String,
    pub context_files: Vec<String>,
    #[serde(default)]
    pub created_by: Option<String>,
    #[serde(default)]
    pub planned_by: Option<String>,
    #[serde(default)]
    pub implemented_by: Option<String>,
    pub status: TaskStatus,
    pub priority: TaskPriority,
    #[serde(default)]
    pub complexity: Option<TaskComplexity>,
    pub task_type: TaskType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_status: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_refs: Vec<ExternalRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<TaskRelation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_run_id: Option<String>,
    /// Trusted execution location of the linked run; legacy links are unknown.
    ///
    /// [ORB-12725] `job_run_host` is read for one release so a task bundle
    /// written by an older build still loads; only the new name is written.
    #[serde(
        default,
        alias = "job_run_host",
        skip_serializing_if = "Option::is_none"
    )]
    pub job_run_machine: Option<ExecutionLocation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
    /// Explicit named crew that owns orchestration of this task. This is
    /// attribution metadata only; execution resolution continues to use `crew`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Display for Task {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}\t{}\t{}\t{}",
            self.id, self.status, self.priority, self.title
        )
    }
}

impl Task {
    pub fn github_pr_number(&self) -> Option<&str> {
        self.external_refs
            .iter()
            .find(|external_ref| external_ref.system == GITHUB_PR_EXTERNAL_REF_SYSTEM)
            .map(|external_ref| external_ref.id.as_str())
    }

    pub fn parent_id(&self) -> Option<&str> {
        self.relation_target(TaskRelationType::ChildOf)
    }

    pub fn dependencies(&self) -> Vec<OrbitId> {
        self.relation_targets(TaskRelationType::BlockedBy)
    }

    pub fn source_task_id(&self) -> Option<&str> {
        self.relation_target(TaskRelationType::RegressionFrom)
    }

    fn relation_target(&self, relation_type: TaskRelationType) -> Option<&str> {
        self.relations
            .iter()
            .find(|relation| relation.relation_type == relation_type)
            .map(|relation| relation.target.as_str())
    }

    fn relation_targets(&self, relation_type: TaskRelationType) -> Vec<OrbitId> {
        self.relations
            .iter()
            .filter(|relation| relation.relation_type == relation_type)
            .map(|relation| relation.target.clone())
            .collect()
    }
}

pub fn normalize_task_dependencies(
    raw_dependencies: Vec<String>,
) -> Result<Vec<OrbitId>, TaskError> {
    let mut normalized = Vec::with_capacity(raw_dependencies.len());
    let mut seen = BTreeSet::new();
    for raw in raw_dependencies {
        let dependency = raw.trim();
        if dependency.is_empty() {
            return Err(TaskError::Invalid(
                "task dependencies must not contain empty IDs".to_string(),
            ));
        }
        if seen.insert(dependency.to_string()) {
            normalized.push(dependency.to_string());
        }
    }
    Ok(normalized)
}

pub fn normalize_task_tags(raw_tags: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::with_capacity(raw_tags.len());
    let mut seen = BTreeSet::new();
    for raw in raw_tags {
        let tag = raw.trim().to_lowercase();
        if !tag.is_empty() && seen.insert(tag.clone()) {
            normalized.push(tag);
        }
    }
    normalized
}

/// Canonical persisted ordering for task-scoped tool requirements.
///
/// Names are deliberately not trimmed or case-folded here: admission owns
/// validating exact canonical registry names, and malformed values must remain
/// visible until that fail-closed check. A sorted set makes every task store
/// and transport serialize the same deduplicated list.
pub fn normalize_required_tools(raw_tools: Vec<String>) -> Vec<String> {
    raw_tools
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub fn deserialize_required_tools<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Vec::<String>::deserialize(deserializer).map(normalize_required_tools)
}

pub fn task_matches_tags(task: &Task, required_tags: &[String]) -> bool {
    let required_tags = normalize_task_tags(required_tags.to_vec());
    if required_tags.is_empty() {
        return true;
    }

    let available = normalize_task_tags(task.tags.clone())
        .into_iter()
        .collect::<BTreeSet<_>>();
    required_tags
        .iter()
        .all(|tag| available.contains(tag.as_str()))
}

pub fn build_task_status_index(tasks: &[Task]) -> BTreeMap<OrbitId, TaskStatus> {
    tasks
        .iter()
        .map(|task| (task.id.clone(), task.status))
        .collect::<BTreeMap<_, _>>()
}

pub fn resolve_task_dependencies(
    task: &Task,
    status_by_id: &BTreeMap<OrbitId, TaskStatus>,
) -> Vec<ResolvedTaskDependency> {
    let reference_index = TaskReferenceIndex::from_status_index(status_by_id);
    resolve_task_dependencies_with_index(task, status_by_id, &reference_index)
}

/// Resolve dependency targets using prefix knowledge derived from one status
/// projection. Callers that inspect several tasks from the same projection
/// should create one [`TaskReferenceIndex`] and reuse it.
pub fn resolve_task_dependencies_with_index(
    task: &Task,
    status_by_id: &BTreeMap<OrbitId, TaskStatus>,
    reference_index: &TaskReferenceIndex,
) -> Vec<ResolvedTaskDependency> {
    task.dependencies()
        .into_iter()
        .map(|dependency_id| ResolvedTaskDependency {
            id: dependency_id.clone(),
            status: status_by_id
                .get(&dependency_id)
                .map(|status| status.to_string())
                .unwrap_or_else(|| {
                    if reference_index.is_not_verifiable_here(task, &dependency_id, status_by_id) {
                        TASK_REFERENCE_NOT_VERIFIABLE_HERE.to_string()
                    } else {
                        "missing".to_string()
                    }
                }),
        })
        .collect()
}

/// Project typed relations with a marker on unresolved foreign-prefix task
/// targets. Resolvable and locally missing targets retain their stored shape.
pub fn resolve_task_relations(
    task: &Task,
    status_by_id: &BTreeMap<OrbitId, TaskStatus>,
) -> Vec<ResolvedTaskRelation> {
    let reference_index = TaskReferenceIndex::from_status_index(status_by_id);
    resolve_task_relations_with_index(task, status_by_id, &reference_index)
}

/// Resolve relation targets using prefix knowledge derived from one status
/// projection. Callers that inspect several tasks from the same projection
/// should create one [`TaskReferenceIndex`] and reuse it.
pub fn resolve_task_relations_with_index(
    task: &Task,
    status_by_id: &BTreeMap<OrbitId, TaskStatus>,
    reference_index: &TaskReferenceIndex,
) -> Vec<ResolvedTaskRelation> {
    task.relations
        .iter()
        .map(|relation| ResolvedTaskRelation {
            relation_type: relation.relation_type,
            target: relation.target.clone(),
            verification: reference_index
                .is_not_verifiable_here(task, &relation.target, status_by_id)
                .then(|| TASK_REFERENCE_NOT_VERIFIABLE_HERE.to_string()),
        })
        .collect()
}

/// Prefix knowledge derived from one bounded status-index snapshot.
///
/// This is intentionally an owned, short-lived value rather than a global
/// cache: task stores may change between snapshots, and the snapshot's caller
/// already defines the correct freshness boundary.
#[derive(Debug)]
pub struct TaskReferenceIndex {
    known_prefixes: BTreeSet<String>,
    indexed_task_count: usize,
}

impl TaskReferenceIndex {
    /// Build prefix knowledge with one scan of a status-index snapshot.
    pub fn from_status_index(status_by_id: &BTreeMap<OrbitId, TaskStatus>) -> Self {
        Self {
            known_prefixes: status_by_id
                .keys()
                .filter_map(|id| task_id_prefix(id).map(ToOwned::to_owned))
                .collect(),
            indexed_task_count: status_by_id.len(),
        }
    }

    /// Number of task IDs examined while building this snapshot's prefix set.
    /// This provides bounded-work evidence without depending on wall-clock time.
    pub fn indexed_task_count(&self) -> usize {
        self.indexed_task_count
    }

    /// Whether a missing valid task target belongs to a prefix this snapshot
    /// cannot verify. The source task's prefix is local even when its task ID
    /// is absent from the projection.
    pub fn is_not_verifiable_here(
        &self,
        task: &Task,
        target: &str,
        status_by_id: &BTreeMap<OrbitId, TaskStatus>,
    ) -> bool {
        if status_by_id.contains_key(target) || !is_valid_orb_task_id(target) {
            return false;
        }
        let Some(target_prefix) = task_id_prefix(target) else {
            return false;
        };
        if task_id_prefix(&task.id).is_some_and(|source_prefix| source_prefix == target_prefix) {
            return false;
        }
        !self.known_prefixes.contains(target_prefix)
    }
}

/// Whether a missing valid task target belongs to a prefix this status
/// projection cannot verify. The source prefix is always local for its task;
/// prefixes on projected task IDs cover additional locally registered legacy
/// or migrated partitions.
pub fn task_reference_is_not_verifiable_here(
    task: &Task,
    target: &str,
    status_by_id: &BTreeMap<OrbitId, TaskStatus>,
) -> bool {
    TaskReferenceIndex::from_status_index(status_by_id).is_not_verifiable_here(
        task,
        target,
        status_by_id,
    )
}

pub fn task_dependencies_ready(task: &Task, status_by_id: &BTreeMap<OrbitId, TaskStatus>) -> bool {
    let reference_index = TaskReferenceIndex::from_status_index(status_by_id);
    task_dependencies_ready_with_index(task, status_by_id, &reference_index)
}

/// Check readiness using prefix knowledge derived from one status projection.
pub fn task_dependencies_ready_with_index(
    task: &Task,
    status_by_id: &BTreeMap<OrbitId, TaskStatus>,
    reference_index: &TaskReferenceIndex,
) -> bool {
    task.dependencies().iter().all(|dependency_id| {
        status_by_id
            .get(dependency_id)
            .is_some_and(|status| status.satisfies_dependency())
            || reference_index.is_not_verifiable_here(task, dependency_id, status_by_id)
    })
}

pub fn unmet_task_dependencies(
    task: &Task,
    status_by_id: &BTreeMap<OrbitId, TaskStatus>,
) -> Vec<ResolvedTaskDependency> {
    let reference_index = TaskReferenceIndex::from_status_index(status_by_id);
    unmet_task_dependencies_with_index(task, status_by_id, &reference_index)
}

/// Return unmet dependencies using prefix knowledge derived from one status
/// projection.
pub fn unmet_task_dependencies_with_index(
    task: &Task,
    status_by_id: &BTreeMap<OrbitId, TaskStatus>,
    reference_index: &TaskReferenceIndex,
) -> Vec<ResolvedTaskDependency> {
    resolve_task_dependencies_with_index(task, status_by_id, reference_index)
        .into_iter()
        .filter(|dependency| {
            !reference_index.is_not_verifiable_here(task, &dependency.id, status_by_id)
                && status_by_id
                    .get(&dependency.id)
                    .is_none_or(|status| !status.satisfies_dependency())
        })
        .collect()
}

/// Returns the task's dependency edges that can never be satisfied by waiting.
///
/// Empty means every unmet edge is a legitimate wait, so the caller should
/// keep polling. Non-empty means polling is futile and the caller should fail
/// now, naming these edges.
pub fn unsatisfiable_task_dependencies(
    task: &Task,
    status_by_id: &BTreeMap<OrbitId, TaskStatus>,
) -> Vec<UnsatisfiableTaskDependency> {
    let reference_index = TaskReferenceIndex::from_status_index(status_by_id);
    unsatisfiable_task_dependencies_with_index(task, status_by_id, &reference_index)
}

/// Return dependency dead ends using prefix knowledge derived from one status
/// projection.
pub fn unsatisfiable_task_dependencies_with_index(
    task: &Task,
    status_by_id: &BTreeMap<OrbitId, TaskStatus>,
    reference_index: &TaskReferenceIndex,
) -> Vec<UnsatisfiableTaskDependency> {
    task.dependencies()
        .into_iter()
        .filter_map(|dependency_id| {
            if reference_index.is_not_verifiable_here(task, &dependency_id, status_by_id) {
                return None;
            }
            let (status, reason) = match status_by_id.get(&dependency_id) {
                None => ("missing".to_string(), DependencyDeadEnd::Missing),
                Some(status) => (status.to_string(), status.dependency_dead_end()?),
            };
            Some(UnsatisfiableTaskDependency {
                task_id: task.id.clone(),
                dependency_id,
                status,
                reason,
            })
        })
        .collect()
}

pub fn validate_task_dependencies(
    tasks: &[Task],
    current_task_id: Option<&str>,
    dependencies: &[OrbitId],
) -> Result<(), TaskError> {
    validate_task_dependencies_with(current_task_id, dependencies, |id| {
        Ok(tasks
            .iter()
            .find(|task| task.id == id)
            .map(Task::dependencies))
    })
}

/// Cycle-check new dependency edges using an on-demand adjacency lookup.
///
/// `lookup(id)` returns that task's current dependency IDs, or `None` when the
/// id is unknown. Unknown targets are treated as leaves (no further edges),
/// matching an adjacency-map miss. Lookups are cached so each id is resolved
/// at most once. The task being updated is not looked up; its outgoing edges
/// are the `dependencies` argument.
pub fn validate_task_dependencies_with<F, E>(
    current_task_id: Option<&str>,
    dependencies: &[OrbitId],
    mut lookup: F,
) -> Result<(), E>
where
    F: FnMut(&str) -> Result<Option<Vec<OrbitId>>, E>,
    E: From<TaskError>,
{
    let Some(current_task_id) = current_task_id else {
        return Ok(());
    };

    if dependencies
        .iter()
        .any(|dependency| dependency == current_task_id)
    {
        return Err(TaskError::Invalid(format!(
            "task '{current_task_id}' cannot declare a self-dependency (self-reference)"
        ))
        .into());
    }

    let mut resolved = BTreeMap::new();
    resolved.insert(current_task_id.to_string(), dependencies.to_vec());

    for dependency in dependencies {
        let mut visiting = BTreeSet::new();
        let mut trail = Vec::new();
        if let Some(path) = find_dependency_path(
            dependency,
            current_task_id,
            &mut resolved,
            &mut lookup,
            &mut visiting,
            &mut trail,
        )? {
            let mut cycle = Vec::with_capacity(path.len() + 1);
            cycle.push(current_task_id.to_string());
            cycle.extend(path);
            return Err(TaskError::Invalid(format!(
                "task dependency cycle detected: {}",
                cycle.join(" -> ")
            ))
            .into());
        }
    }

    Ok(())
}

fn find_dependency_path<F, E>(
    current: &str,
    target: &str,
    resolved: &mut BTreeMap<OrbitId, Vec<OrbitId>>,
    lookup: &mut F,
    visiting: &mut BTreeSet<OrbitId>,
    trail: &mut Vec<OrbitId>,
) -> Result<Option<Vec<OrbitId>>, E>
where
    F: FnMut(&str) -> Result<Option<Vec<OrbitId>>, E>,
{
    if !visiting.insert(current.to_string()) {
        return Ok(None);
    }

    trail.push(current.to_string());
    if current == target {
        return Ok(Some(trail.clone()));
    }

    if !resolved.contains_key(current) {
        resolved.insert(current.to_string(), lookup(current)?.unwrap_or_default());
    }
    let next_dependencies = resolved.get(current).cloned().unwrap_or_default();
    for next in &next_dependencies {
        if let Some(path) = find_dependency_path(next, target, resolved, lookup, visiting, trail)? {
            return Ok(Some(path));
        }
    }

    trail.pop();
    visiting.remove(current);
    Ok(None)
}

/// Canonical automatic admission order, shared by reporting and the owner store.
pub fn automatic_dispatch_cmp(left: &Task, right: &Task) -> std::cmp::Ordering {
    let band = |task: &Task| {
        if task.priority == TaskPriority::Critical {
            0
        } else if task.task_type == TaskType::Bug
            || task
                .tags
                .iter()
                .any(|tag| matches!(tag.as_str(), "code-review" | "security-review"))
        {
            1
        } else {
            2
        }
    };
    let priority = |value| match value {
        TaskPriority::Critical => 0,
        TaskPriority::High => 1,
        TaskPriority::Medium => 2,
        TaskPriority::Low => 3,
    };
    band(left)
        .cmp(&band(right))
        .then(priority(left.priority).cmp(&priority(right.priority)))
        .then(left.created_at.cmp(&right.created_at))
        .then(left.id.cmp(&right.id))
}

/// Persisted execution location. Absence on older records means unknown.
///
/// A machine's display name is metadata; only the stable machine id identifies
/// execution. [ORB-12725] `host_id` is read for one release so a run record
/// written by an older build still loads; only `machine_name` is written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionLocation {
    pub machine_id: String,
    #[serde(default, alias = "host_id")]
    pub machine_name: Option<String>,
}
