use super::*;
use crate::task::TaskEnvelopeV2;

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
    /// Optional admission footprint. An empty list holds no context locks.
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
    /// Permanent read alias [ORB-12725, ORB-18313]: `job_run_host` stays
    /// readable because task bundles written before v0.24 still carry it, and
    /// `TaskEnvelopeV2` denies unknown fields. Only the new name is written.
    #[serde(
        default,
        alias = "job_run_host",
        skip_serializing_if = "Option::is_none"
    )]
    pub job_run_machine: Option<ExecutionLocation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
    /// Assignment provenance: `explicit`, `pool:<complexity>`, or `default`.
    /// Absent on legacy records; assignment history can recover it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew_source: Option<String>,
    /// Explicit named crew that owns orchestration of this task. This is
    /// attribution metadata only; execution resolution continues to use `crew`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator: Option<String>,
    /// The orchestrator's model, snapshotted when the task was created with
    /// an orchestrator: an explicit creation input, else that crew's
    /// configured model. Never rewritten afterwards, so a later orchestrator
    /// change or crew-config edit leaves it as recorded. Absent on tasks
    /// created without an orchestrator and on tasks filed before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator_model: Option<String>,
    /// Which rule supplied `orchestrator_model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator_model_source: Option<OrchestratorModelSource>,
    /// Why the task was filed. Absent on tasks filed before origins were
    /// recorded and on interactive filings that did not state one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<TaskOrigin>,
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
    /// Digest of what a candidate implementation answers to: the description
    /// and acceptance criteria. A preserved or held candidate is resumed only
    /// while this is unchanged [ORB-13985]. Context selectors are preparation
    /// hints, not what the task means, so editing them keeps the candidate
    /// [ORB-14450].
    pub fn spec_digest(&self) -> String {
        use crate::identity::lower_hex;
        use sha2::{Digest, Sha256};
        let spec = serde_json::json!({
            "description": self.description,
            "acceptance_criteria": self.acceptance_criteria,
        });
        lower_hex(Sha256::digest(spec.to_string().as_bytes()))
    }

    /// Whether a recorded [`Self::spec_digest`] still describes this task.
    /// Digests recorded before [ORB-14450] also covered the selectors; they
    /// match while the selectors are unchanged too.
    pub fn spec_digest_matches(&self, recorded: &str) -> bool {
        use crate::identity::lower_hex;
        use sha2::{Digest, Sha256};
        if recorded == self.spec_digest() {
            return true;
        }
        let mut selectors = self.context_files.iter().collect::<Vec<_>>();
        selectors.sort();
        selectors.dedup();
        let legacy = serde_json::json!({
            "description": self.description,
            "acceptance_criteria": self.acceptance_criteria,
            "context_files": selectors,
        });
        recorded == lower_hex(Sha256::digest(legacy.to_string().as_bytes()))
    }

    /// The task an envelope and its body documents describe.
    ///
    /// Passing empty bodies yields the metadata-only view: `description`,
    /// `acceptance_criteria`, `plan` and `execution_summary` are empty while
    /// every envelope field is populated exactly as a full read populates it.
    pub fn from_envelope_parts(
        envelope: TaskEnvelopeV2,
        description: String,
        acceptance_criteria: Vec<String>,
        plan: String,
        execution_summary: String,
    ) -> Self {
        let status = envelope.status;
        Self {
            job_run_machine: envelope.job_run_machine,
            id: envelope.id,
            title: envelope.title,
            description,
            acceptance_criteria,
            tags: normalize_task_tags(envelope.tags),
            required_tools: normalize_required_tools(envelope.required_tools),
            plan,
            execution_summary,
            context_files: envelope.context_files,
            created_by: envelope.created_by,
            planned_by: envelope.planned_by,
            implemented_by: envelope.implemented_by,
            status,
            priority: envelope.priority,
            complexity: envelope.complexity,
            task_type: envelope.task_type,
            pr_status: envelope.pr_status,
            external_refs: envelope.external_refs,
            relations: envelope.relations,
            job_run_id: envelope.job_run_id,
            crew: envelope.crew,
            crew_source: envelope.crew_source,
            orchestrator: envelope.orchestrator,
            orchestrator_model: envelope.orchestrator_model,
            orchestrator_model_source: envelope.orchestrator_model_source,
            origin: envelope.origin,
            created_at: envelope.created_at,
            updated_at: envelope.updated_at,
        }
    }

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

    /// The delivery job this task selects with a `delivery:<job>` tag, if any.
    /// See [`delivery_job_selection`].
    pub fn delivery_job_selection(&self) -> Result<Option<&str>, TaskError> {
        delivery_job_selection(&self.tags)
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

/// Prefix of system identity tags assigned to CI failure tracking tasks.
pub const CI_FAILURE_KEY_TAG_PREFIX: &str = "ci-failure:";

/// Whether a tag is a system identity tag that tracks automated system provenance.
pub fn is_system_identity_tag(tag: &str) -> bool {
    tag.trim()
        .to_ascii_lowercase()
        .starts_with(CI_FAILURE_KEY_TAG_PREFIX)
}

/// Tag prefix a task uses to select the job that delivers it when shipped.
///
/// `delivery:<job>` names a catalog job that declares `spec.task_delivery`
/// for the ship mode; the gate dispatches it instead of
/// `task_<mode>_pipeline`. A task without the tag ships as before.
pub const DELIVERY_JOB_TAG_PREFIX: &str = "delivery:";

/// The job named by a `delivery:<job>` tag among `tags`.
///
/// Repeating the same tag is harmless; two different selections, or a tag with
/// no job name, are refused rather than resolved by order, because either
/// choice would silently ship the task through a job nobody picked.
pub fn delivery_job_selection(tags: &[String]) -> Result<Option<&str>, TaskError> {
    let mut selected: Option<&str> = None;
    for tag in tags {
        let Some(job) = tag.trim().strip_prefix(DELIVERY_JOB_TAG_PREFIX) else {
            continue;
        };
        let job = job.trim();
        if job.is_empty() {
            return Err(TaskError::Invalid(format!(
                "tag '{tag}' selects no delivery job; use `{DELIVERY_JOB_TAG_PREFIX}<job>`"
            )));
        }
        match selected {
            Some(existing) if existing != job => {
                return Err(TaskError::Invalid(format!(
                    "tags select two delivery jobs, '{existing}' and '{job}'; keep one \
                     `{DELIVERY_JOB_TAG_PREFIX}<job>` tag"
                )));
            }
            _ => selected = Some(job),
        }
    }
    Ok(selected)
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
            regression: relation.regression,
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

/// Whether an archived task reached `done` before it was archived, so a
/// `blocked_by` edge onto it is satisfied exactly like an edge onto `done`.
///
/// Archive is a soft delete reachable from any status: the status alone
/// cannot tell finished work from abandoned work, but the history can.
/// Reaching `done` completes the task; any later move to a status other than
/// `done` or `archived` (a reopen, a restore, a rejection) takes the
/// completion back. The history must end in the archive, so a history that
/// disagrees with the archived projection keeps the dead end.
pub fn archived_task_completed_before_archive(history: &[TaskHistoryEntry]) -> bool {
    let mut completed = false;
    let mut last = None;
    for status in history.iter().filter_map(|entry| entry.to_status) {
        match status {
            TaskStatus::Done => completed = true,
            TaskStatus::Archived => {}
            _ => completed = false,
        }
        last = Some(status);
    }
    completed && last == Some(TaskStatus::Archived)
}

/// Rewrite each archived dependency target that reached `done` before it was
/// archived ([`archived_task_completed_before_archive`]) to `done` in a
/// dependency status projection.
///
/// Every surface that computes dependency satisfaction — dispatch admission,
/// readiness and promotion, and the `resolved_dependencies` projection —
/// applies this to its status projection first, so they share one rule.
/// Only archived targets among `dependencies` are looked up through
/// `history_of`; a target without readable history (`None`) stays archived
/// and remains a dead end.
pub fn satisfy_completed_archived_dependencies<E>(
    status_by_id: &mut BTreeMap<OrbitId, TaskStatus>,
    dependencies: impl IntoIterator<Item = OrbitId>,
    mut history_of: impl FnMut(&str) -> Result<Option<Vec<TaskHistoryEntry>>, E>,
) -> Result<(), E> {
    let archived = dependencies
        .into_iter()
        .filter(|id| status_by_id.get(id) == Some(&TaskStatus::Archived))
        .collect::<BTreeSet<_>>();
    for id in archived {
        if history_of(&id)?.is_some_and(|history| archived_task_completed_before_archive(&history))
        {
            status_by_id.insert(id, TaskStatus::Done);
        }
    }
    Ok(())
}

/// Cycle-check new dependency edges using an on-demand adjacency lookup.
///
/// `lookup(id)` returns that task's current dependency IDs, or `None` when the
/// id is unknown. Unknown targets are treated as leaves (no further edges),
/// matching an adjacency-map miss. Each reachable id is looked up at most once
/// and examined once, so validation work is bounded by the reachable
/// vertices and edges. The task being updated is not looked up; its outgoing
/// edges are the `dependencies` argument.
pub fn validate_task_dependencies_with<F, E>(
    current_task_id: Option<&str>,
    dependencies: &[OrbitId],
    lookup: F,
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

    match walk_dependencies_to_self(current_task_id, dependencies, lookup)?.cycle {
        Some(cycle) => Err(TaskError::Invalid(format!(
            "task dependency cycle detected: {}",
            cycle.join(" -> ")
        ))
        .into()),
        None => Ok(()),
    }
}

/// Result of one dependency walk from the updated task.
struct DependencyWalk {
    /// Cycle witness from the updated task back to itself, when one exists.
    cycle: Option<Vec<OrbitId>>,
}

/// Depth-first search for a path from `dependencies` back to `current_task_id`.
///
/// Iterative so long chains cannot exhaust the stack. A node is expanded at
/// most once: a node already expanded either finished without reaching the
/// updated task or is still on the active path, whose exploration covers
/// everything it reaches. Cycles that do not pass through the updated task are
/// ignored, as they predate this edit.
fn walk_dependencies_to_self<F, E>(
    current_task_id: &str,
    dependencies: &[OrbitId],
    mut lookup: F,
) -> Result<DependencyWalk, E>
where
    F: FnMut(&str) -> Result<Option<Vec<OrbitId>>, E>,
{
    let mut expanded = BTreeSet::new();
    let mut path = vec![current_task_id.to_string()];
    let mut pending = vec![Vec::from(dependencies).into_iter()];

    while let Some(next_edges) = pending.last_mut() {
        let Some(next) = next_edges.next() else {
            pending.pop();
            path.pop();
            continue;
        };
        if next == current_task_id {
            path.push(next);
            return Ok(DependencyWalk { cycle: Some(path) });
        }
        if !expanded.insert(next.clone()) {
            continue;
        }
        let adjacency = lookup(&next)?.unwrap_or_default();
        path.push(next);
        pending.push(adjacency.into_iter());
    }

    Ok(DependencyWalk { cycle: None })
}

/// Canonical automatic admission order, shared by reporting and the owner store.
pub fn automatic_dispatch_cmp(left: &Task, right: &Task) -> std::cmp::Ordering {
    automatic_dispatch_cmp_with_expiry(left, false, right, false)
}

/// [`automatic_dispatch_cmp`] with each task's frozen-batch expiry
/// [ORB-14624]. An expiring task — one whose frozen delivery batch nears its
/// admission deadline — joins the corrective band and leads its priority
/// there, so it sorts ahead of every same-priority task outside the critical
/// band. Critical work still leads, and a higher-priority corrective task
/// still sorts first.
pub fn automatic_dispatch_cmp_with_expiry(
    left: &Task,
    left_expiring: bool,
    right: &Task,
    right_expiring: bool,
) -> std::cmp::Ordering {
    automatic_dispatch_cmp_with_affinity(
        (left, left_expiring, DispatchAffinity::default()),
        (right, right_expiring, DispatchAffinity::default()),
        AdmissionAffinityReach::Off,
    )
}

/// How far work only the requesting executor can run reaches in automatic
/// dispatch order [ORB-17304]: the `workflow.admission_affinity` setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionAffinityReach {
    /// Exclusive work leads its band, ahead of higher-priority work other
    /// executors can run. Critical and corrective work still leads.
    #[default]
    Band,
    /// Exclusive work leads only among tasks of its own priority.
    Priority,
    /// No affinity: the canonical band, priority, expiry, age and ID order.
    Off,
}

impl AdmissionAffinityReach {
    /// Every accepted spelling, in declaration order.
    pub const CHOICES: [&'static str; 3] = ["band", "priority", "off"];

    /// The reach a setting spells, or `None` for any other value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "band" => Some(Self::Band),
            "priority" => Some(Self::Priority),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

/// One candidate's standing against the live executor census, as the
/// requesting executor sees it [ORB-17304]. The default is the standing every
/// candidate shares when no census applies, so it never reorders.
/// Neutral usage pressure, in milli-units (1000 = pressure 1.0), as an
/// admission ordering key [ORB-17773].
pub const NEUTRAL_PRESSURE_MILLI: u64 = 1000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DispatchAffinity {
    /// The requester is the only live executor that can run the task.
    pub exclusive: bool,
    /// How many live executors can run the task; fewer sorts first. A task
    /// the requester itself cannot run carries `usize::MAX`, so it sorts
    /// after every task the requester can.
    pub eligible: usize,
    /// The highest weekly usage pressure, in milli-units, the requester
    /// reported for a member of the task's crew pool [ORB-17773]; higher
    /// sorts first, below every affinity key and above age. The default
    /// `0` is shared by every candidate of a pressure-free order, so it
    /// never reorders.
    pub pressure_milli: u64,
}

/// Automatic dispatch order for one requesting executor against the live
/// executor census [ORB-17304].
///
/// `band → exclusive → priority → expiry → fewer eligible executors → usage
/// pressure → age → ID` under [`AdmissionAffinityReach::Band`]; exclusivity
/// moves after priority under [`AdmissionAffinityReach::Priority`], and
/// [`AdmissionAffinityReach::Off`] ignores both affinity keys. Each side is a
/// task, whether its frozen batch is expiring, and its affinity. Exclusivity
/// never crosses a band, so critical and corrective work still leads. Usage
/// pressure [ORB-17773] is the lowest-precedence key above age and ID: it
/// only reorders tasks that tie on band, priority, expiry and affinity, it
/// applies under every reach, and a requester that reported none leaves
/// every candidate at the shared default, so the order is unchanged.
pub fn automatic_dispatch_cmp_with_affinity(
    left: (&Task, bool, DispatchAffinity),
    right: (&Task, bool, DispatchAffinity),
    reach: AdmissionAffinityReach,
) -> std::cmp::Ordering {
    let band = |task: &Task, expiring: bool| {
        if task.priority == TaskPriority::Critical {
            0
        } else if expiring
            || task.task_type == TaskType::Bug
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
    let (left, left_expiring, left_affinity) = left;
    let (right, right_expiring, right_affinity) = right;
    let (left_affinity, right_affinity) = match reach {
        // `Off` drops the affinity keys only; usage pressure is a separate,
        // lowest-precedence key the reach does not govern [ORB-17773].
        AdmissionAffinityReach::Off => (
            DispatchAffinity {
                pressure_milli: left_affinity.pressure_milli,
                ..DispatchAffinity::default()
            },
            DispatchAffinity {
                pressure_milli: right_affinity.pressure_milli,
                ..DispatchAffinity::default()
            },
        ),
        _ => (left_affinity, right_affinity),
    };
    let exclusive_at = |at: AdmissionAffinityReach| {
        if reach == at {
            right_affinity.exclusive.cmp(&left_affinity.exclusive)
        } else {
            std::cmp::Ordering::Equal
        }
    };
    band(left, left_expiring)
        .cmp(&band(right, right_expiring))
        .then(exclusive_at(AdmissionAffinityReach::Band))
        .then(priority(left.priority).cmp(&priority(right.priority)))
        .then(exclusive_at(AdmissionAffinityReach::Priority))
        .then(right_expiring.cmp(&left_expiring))
        .then(left_affinity.eligible.cmp(&right_affinity.eligible))
        .then(
            right_affinity
                .pressure_milli
                .cmp(&left_affinity.pressure_milli),
        )
        .then(left.created_at.cmp(&right.created_at))
        .then(left.id.cmp(&right.id))
}

/// Persisted execution location. Absence on older records means unknown.
///
/// A machine's display name is metadata; only the stable machine id identifies
/// execution. Permanent read alias [ORB-12725, ORB-18313]: `host_id` stays
/// readable for run records written before v0.24; only `machine_name` is
/// written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionLocation {
    pub machine_id: String,
    #[serde(default, alias = "host_id")]
    pub machine_name: Option<String>,
}
