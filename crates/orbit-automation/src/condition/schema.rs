//! The condition language's declared environment: the variables each scope
//! binds and every field an expression may read
//! (`docs/design/condition-triggers/1_scope.md` §2), and the typed wait
//! environment of a job's auto-task steps
//! (`docs/design/activity-job/specs/auto-task-step.md` §2). Nothing outside
//! these tables is in the schema.

use std::collections::{BTreeMap, BTreeSet};

use orbit_types::task::TaskStatus;
use orbit_types::workflow::automation::ConditionScope;

/// A type in the admitted subset. There is no `dyn`: every expression has
/// one static type, and `Unknown` only stands for the element of an empty
/// list literal until it unifies with something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CelType {
    Bool,
    Int,
    Uint,
    Double,
    String,
    Bytes,
    Null,
    Timestamp,
    Duration,
    List(Box<CelType>),
    Map(Box<CelType>, Box<CelType>),
    Object(ObjectKind),
    Unknown,
}

impl CelType {
    pub(crate) fn name(&self) -> String {
        match self {
            Self::Bool => "bool".into(),
            Self::Int => "int".into(),
            Self::Uint => "uint".into(),
            Self::Double => "double".into(),
            Self::String => "string".into(),
            Self::Bytes => "bytes".into(),
            Self::Null => "null".into(),
            Self::Timestamp => "timestamp".into(),
            Self::Duration => "duration".into(),
            Self::List(element) => format!("list({})", element.name()),
            Self::Map(key, value) => format!("map({}, {})", key.name(), value.name()),
            Self::Object(kind) => kind.name().into(),
            Self::Unknown => "unknown".into(),
        }
    }

    pub(crate) fn is_numeric(&self) -> bool {
        matches!(self, Self::Int | Self::Uint | Self::Double)
    }
}

/// The object shapes of the schema. `Tasks` and `TagCounts` are maps an
/// expression may only index with a string literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObjectKind {
    Task,
    Origin,
    Board,
    Bucket,
    SelfRoutine,
    SelfAutoTask,
    Tasks,
    TagCounts,
    /// `board` in a job wait: the condition board plus `empty`.
    JobBoard,
    /// `job` whose `source_revision` may be absent.
    Job,
    /// `job` of a job that pins its source revision at admission.
    PinnedJob,
    /// `self` of an auto-task step.
    SelfStep,
    /// `self.prev`, the previous sibling.
    PrevStep,
    /// `steps`, indexed only by a string-literal step ID.
    Steps,
    /// One `steps['<id>']` observation.
    StepObservation,
    /// `result` of a `reuse_when`.
    Result,
    /// `result` under a `release-readiness/v1` contract, which carries the
    /// source revision it checked.
    ReadinessResult,
}

impl ObjectKind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Origin => "origin",
            Self::Board => "board",
            Self::Bucket => "board count",
            Self::SelfRoutine | Self::SelfAutoTask => "self",
            Self::Tasks => "tasks",
            Self::TagCounts => "board.by_tag",
            Self::JobBoard => "board",
            Self::Job | Self::PinnedJob => "job",
            Self::SelfStep => "self",
            Self::PrevStep => "self.prev",
            Self::Steps => "steps",
            Self::StepObservation => "step",
            Self::Result | Self::ReadinessResult => "result",
        }
    }

    /// The fields a selection may name. Literal-indexed maps have none.
    pub(crate) fn fields(self) -> &'static [Field] {
        match self {
            Self::Task => TASK_FIELDS,
            Self::Origin => ORIGIN_FIELDS,
            Self::Board => BOARD_FIELDS,
            Self::Bucket => BUCKET_FIELDS,
            Self::SelfRoutine => SELF_ROUTINE_FIELDS,
            Self::SelfAutoTask => SELF_AUTO_TASK_FIELDS,
            Self::JobBoard => JOB_BOARD_FIELDS,
            Self::Job => JOB_FIELDS,
            Self::PinnedJob => PINNED_JOB_FIELDS,
            Self::SelfStep => SELF_STEP_FIELDS,
            Self::PrevStep => PREV_STEP_FIELDS,
            Self::StepObservation => STEP_OBSERVATION_FIELDS,
            Self::Result => RESULT_FIELDS,
            Self::ReadinessResult => READINESS_RESULT_FIELDS,
            Self::Tasks | Self::TagCounts | Self::Steps => &[],
        }
    }

    /// Whether this object is a `board`, whose field reads are recorded.
    pub(crate) fn is_board(self) -> bool {
        matches!(self, Self::Board | Self::JobBoard)
    }

    /// The status spellings this object's `status` takes, if it has one.
    pub(crate) fn statuses(self) -> Option<Statuses> {
        match self {
            Self::Task => Some(Statuses::Task),
            Self::SelfStep | Self::PrevStep | Self::StepObservation => Some(Statuses::Step),
            Self::ReadinessResult => Some(Statuses::Result),
            _ => None,
        }
    }

    pub(crate) fn field(self, name: &str) -> Option<&'static Field> {
        self.fields().iter().find(|field| field.name == name)
    }
}

/// A field's static type, spelled so the tables can be `const`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FieldType {
    String,
    Int,
    Bool,
    Timestamp,
    StringList,
    StringIntMap,
    Object(ObjectKind),
}

impl FieldType {
    pub(crate) fn cel_type(self) -> CelType {
        match self {
            Self::String => CelType::String,
            Self::Int => CelType::Int,
            Self::Bool => CelType::Bool,
            Self::Timestamp => CelType::Timestamp,
            Self::StringList => CelType::List(Box::new(CelType::String)),
            Self::StringIntMap => CelType::Map(Box::new(CelType::String), Box::new(CelType::Int)),
            Self::Object(kind) => CelType::Object(kind),
        }
    }
}

/// One readable field. An optional field may be absent and must be tested
/// with `has()` before it is read.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Field {
    pub(crate) name: &'static str,
    pub(crate) ty: FieldType,
    pub(crate) optional: bool,
}

const fn required(name: &'static str, ty: FieldType) -> Field {
    Field {
        name,
        ty,
        optional: false,
    }
}

const fn optional(name: &'static str, ty: FieldType) -> Field {
    Field {
        name,
        ty,
        optional: true,
    }
}

const TASK_FIELDS: &[Field] = &[
    required("id", FieldType::String),
    required("title", FieldType::String),
    required("status", FieldType::String),
    required("tags", FieldType::StringList),
    required("type", FieldType::String),
    required("priority", FieldType::String),
    required("complexity", FieldType::String),
    optional("crew", FieldType::String),
    optional("parent_id", FieldType::String),
    required("dependencies", FieldType::StringList),
    required("created_at", FieldType::Timestamp),
    required("updated_at", FieldType::Timestamp),
    required("status_since", FieldType::Timestamp),
    optional("origin", FieldType::Object(ObjectKind::Origin)),
];

const ORIGIN_FIELDS: &[Field] = &[
    required("trigger", FieldType::String),
    required("fire_id", FieldType::String),
    required("depth", FieldType::Int),
];

const BOARD_FIELDS: &[Field] = &[
    required("by_status", FieldType::StringIntMap),
    required("by_tag", FieldType::Object(ObjectKind::TagCounts)),
    required("eligible", FieldType::Object(ObjectKind::Bucket)),
    required("backlog", FieldType::Object(ObjectKind::Bucket)),
    required("in_progress", FieldType::Object(ObjectKind::Bucket)),
    required("blocked", FieldType::Object(ObjectKind::Bucket)),
    required("review", FieldType::Object(ObjectKind::Bucket)),
    required("open_runs", FieldType::Object(ObjectKind::Bucket)),
    required("drain_running", FieldType::Bool),
];

/// The condition board plus `empty`: no backlog, in-progress or review task.
const JOB_BOARD_FIELDS: &[Field] = &[
    required("by_status", FieldType::StringIntMap),
    required("by_tag", FieldType::Object(ObjectKind::TagCounts)),
    required("eligible", FieldType::Object(ObjectKind::Bucket)),
    required("backlog", FieldType::Object(ObjectKind::Bucket)),
    required("in_progress", FieldType::Object(ObjectKind::Bucket)),
    required("blocked", FieldType::Object(ObjectKind::Bucket)),
    required("review", FieldType::Object(ObjectKind::Bucket)),
    required("open_runs", FieldType::Object(ObjectKind::Bucket)),
    required("drain_running", FieldType::Bool),
    required("empty", FieldType::Bool),
];

const JOB_FIELDS: &[Field] = &[
    required("started_at", FieldType::Timestamp),
    optional("source_revision", FieldType::String),
];

const PINNED_JOB_FIELDS: &[Field] = &[
    required("started_at", FieldType::Timestamp),
    required("source_revision", FieldType::String),
];

const SELF_STEP_FIELDS: &[Field] = &[
    required("wait_started_at", FieldType::Timestamp),
    required("status", FieldType::String),
    optional("task_id", FieldType::String),
    optional("prev", FieldType::Object(ObjectKind::PrevStep)),
];

const PREV_STEP_FIELDS: &[Field] = &[
    required("status", FieldType::String),
    optional("end_time", FieldType::Timestamp),
];

const STEP_OBSERVATION_FIELDS: &[Field] = &[
    required("status", FieldType::String),
    optional("end_time", FieldType::Timestamp),
    optional("task_id", FieldType::String),
];

const RESULT_FIELDS: &[Field] = &[
    required("status", FieldType::String),
    optional("revision", FieldType::String),
    required("completed_at", FieldType::Timestamp),
    required("contract", FieldType::String),
    required("definition_digest", FieldType::String),
    required("task_id", FieldType::String),
    optional("run_id", FieldType::String),
];

const READINESS_RESULT_FIELDS: &[Field] = &[
    required("status", FieldType::String),
    required("revision", FieldType::String),
    required("completed_at", FieldType::Timestamp),
    required("contract", FieldType::String),
    required("definition_digest", FieldType::String),
    required("task_id", FieldType::String),
    optional("run_id", FieldType::String),
];

const BUCKET_FIELDS: &[Field] = &[
    required("count", FieldType::Int),
    required("empty", FieldType::Bool),
];

const SELF_ROUTINE_FIELDS: &[Field] = &[
    optional("last_fired", FieldType::Timestamp),
    required("fires_24h", FieldType::Int),
];

const SELF_AUTO_TASK_FIELDS: &[Field] = &[
    optional("last_fired", FieldType::Timestamp),
    optional("last_minted", FieldType::Timestamp),
    optional("last_minted_task", FieldType::String),
    required("fires_24h", FieldType::Int),
];

/// Which definition kind carries the condition: it decides the clause path
/// errors name and which `self` fields exist (only an auto-task mints).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConditionCarrier {
    Routine,
    AutoTask,
}

impl ConditionCarrier {
    /// Where the condition block sits in its file.
    pub fn clause(self) -> &'static str {
        match self {
            Self::Routine => "trigger.condition",
            Self::AutoTask => "schedule.condition",
        }
    }

    /// The definition kind as `explain` and `task.origin.trigger` name it.
    pub fn kind(self) -> &'static str {
        match self {
            Self::Routine => "routine",
            Self::AutoTask => "auto-task",
        }
    }

    fn self_kind(self) -> ObjectKind {
        match self {
            Self::Routine => ObjectKind::SelfRoutine,
            Self::AutoTask => ObjectKind::SelfAutoTask,
        }
    }
}

/// Where an expression is checked: a routine's or auto-task's condition, or
/// one CEL field of a job with auto-task steps.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Environment<'a> {
    Condition {
        scope: ConditionScope,
        carrier: ConditionCarrier,
    },
    JobWait(&'a WaitScope),
}

/// Which job field a wait expression sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaitClause {
    /// `spec.expire_when`: reads `now` and `job` only.
    JobExpiry,
    /// A step's `wait_until` or `expire_when`.
    StepWait,
    /// A step's `reuse_when`, which also reads `result`.
    StepReuse,
}

/// What one job wait expression may read, fixed by the job's structure.
#[derive(Debug, Clone)]
pub(crate) struct WaitScope {
    pub(crate) clause: WaitClause,
    /// The previous sibling in the step's sequential list; `None` for a
    /// first step or a parallel branch, where `self.prev` is always absent.
    pub(crate) prev: Option<String>,
    /// Step IDs a `steps['<id>']` read may name.
    pub(crate) visible_steps: BTreeSet<String>,
    /// Why each other step of the job may not be read from here.
    pub(crate) refused_steps: BTreeMap<String, String>,
    /// Whether the job pins `job.source_revision` at admission.
    pub(crate) pinned_revision: bool,
    /// Whether `result` is a `release-readiness/v1` receipt.
    pub(crate) readiness_result: bool,
}

/// The variables a job wait expression binds.
pub(crate) fn wait_variable(wait: &WaitScope, name: &str) -> Result<CelType, String> {
    let job = if wait.pinned_revision {
        ObjectKind::PinnedJob
    } else {
        ObjectKind::Job
    };
    match (wait.clause, name) {
        (_, "now") => Ok(CelType::Timestamp),
        (_, "job") => Ok(CelType::Object(job)),
        (WaitClause::JobExpiry, _) => Err(format!(
            "unknown identifier '{name}'; spec.expire_when reads now and job only"
        )),
        (_, "self") => Ok(CelType::Object(ObjectKind::SelfStep)),
        (_, "steps") => Ok(CelType::Object(ObjectKind::Steps)),
        (_, "board") => Ok(CelType::Object(ObjectKind::JobBoard)),
        (_, "tasks") => Ok(CelType::Object(ObjectKind::Tasks)),
        (WaitClause::StepReuse, "result") => Ok(CelType::Object(if wait.readiness_result {
            ObjectKind::ReadinessResult
        } else {
            ObjectKind::Result
        })),
        (WaitClause::StepWait, "result") => {
            Err("`result` is read only in reuse_when, where it is the candidate evidence".into())
        }
        _ => Err(format!(
            "unknown identifier '{name}'; a step wait reads self, steps['<id>'], job, board, \
             tasks['<ID>'] and now{}",
            if wait.clause == WaitClause::StepReuse {
                ", and result in reuse_when"
            } else {
                ""
            }
        )),
    }
}

/// The status vocabularies a `status` field can take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Statuses {
    /// A task's stored status.
    Task,
    /// An auto-task step's observed status (spec §2).
    Step,
    /// A `release-readiness/v1` receipt's status (spec §4).
    Result,
}

/// Observed statuses of an auto-task step, as `self.status`,
/// `self.prev.status` and `steps['<id>'].status` evaluate. `done` is an
/// accepted completion; a skipped step stays `skipped`.
pub const AUTO_TASK_STEP_STATUSES: &[&str] = &[
    "waiting",
    "queued",
    "running",
    "done",
    "held",
    "failed",
    "timeout",
    "cancelled",
    "skipped",
];

/// The statuses a `release-readiness/v1` receipt may record.
pub(crate) const READINESS_RESULT_STATUSES: &[&str] = &["passed", "blocked", "unavailable"];

/// The variables one scope binds.
pub(crate) fn variable(
    scope: ConditionScope,
    carrier: ConditionCarrier,
    name: &str,
) -> Option<CelType> {
    match name {
        "task" if scope == ConditionScope::Task => Some(CelType::Object(ObjectKind::Task)),
        "tasks" => Some(CelType::Object(ObjectKind::Tasks)),
        "board" => Some(CelType::Object(ObjectKind::Board)),
        "self" => Some(CelType::Object(carrier.self_kind())),
        "now" => Some(CelType::Timestamp),
        _ => None,
    }
}

/// Why a name in the design's schema is not readable here: a later phase
/// ships it, or this scope binds no such subject.
pub(crate) fn unavailable_variable(scope: ConditionScope, name: &str) -> Option<String> {
    match name {
        "task" => Some(format!(
            "scope {} binds no `task`; read tasks['<ID>'] or board instead",
            scope.as_str()
        )),
        "host" => Some(
            "scope_unavailable: `host` reads ship with host-scope conditions, which this Orbit \
             cannot check yet"
                .into(),
        ),
        "system" | "file" | "group" => Some(format!(
            "scope_unavailable: `{name}` reads ship with file-scope conditions, which this Orbit \
             cannot check yet"
        )),
        _ => None,
    }
}

/// Canonical status spellings, as `task.status` evaluates.
pub(crate) fn status_names() -> Vec<String> {
    TaskStatus::ALL
        .iter()
        .filter_map(|status| {
            serde_json::to_value(status)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
        })
        .collect()
}

pub(crate) fn require_status(literal: &str) -> Result<(), String> {
    require_status_in(Statuses::Task, literal)
}

pub(crate) fn require_status_in(statuses: Statuses, literal: &str) -> Result<(), String> {
    let (kind, names) = match statuses {
        Statuses::Task => ("task", status_names()),
        Statuses::Step => (
            "step",
            AUTO_TASK_STEP_STATUSES
                .iter()
                .map(|status| (*status).to_owned())
                .collect(),
        ),
        Statuses::Result => (
            "result",
            READINESS_RESULT_STATUSES
                .iter()
                .map(|status| (*status).to_owned())
                .collect(),
        ),
    };
    if names.iter().any(|name| name == literal) {
        return Ok(());
    }
    Err(format!(
        "unknown {kind} status '{literal}'; statuses are {}",
        names.join(", ")
    ))
}
