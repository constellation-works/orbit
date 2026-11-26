//! The task app: every task and decision command, whichever surface sent it.
//!
//! A surface builds a [`Command`], calls [`exec`] and renders the
//! [`Outcome`]. `exec` runs through `app/exec.rs`, which authorizes and
//! audits the command; the flows here do neither.
//!
//! A command is one of three kinds:
//!
//! - A verb of the task or decision operation registry, with fields under
//!   that verb's spec: [`Command::Operation`] and [`Command::Decision`]. The
//!   command line, the generic tool entry and the dashboard all send these,
//!   so `orbit task add`, `orbit.task.add` and `POST /api/tasks` end in the
//!   same arm.
//! - A task command that has no registry verb and stays hand-written on the
//!   command line: archive, flow, recheck-blocked, settle-merged, export,
//!   import, reindex and publication.
//! - A read model or a record the dashboard takes: [`Command::Read`] and
//!   [`Command::Record`].
//!
//! Each arm calls the runtime method the surface called before. The verbs'
//! tool flows are the private child modules; they moved here from the tool
//! host and still share its input and JSON helpers.

mod decision;
mod lifecycle;
mod listing;
mod operation;
mod publication;
mod read;
mod show;
mod surface;
mod web;
mod write;

use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_common::governance::decision::DecisionVerb;
use orbit_common::governance::task::TaskVerb;
use orbit_store::TaskCompletionByComplexity;
use orbit_tools::ReservationOwnerContext;
use orbit_types::identity::HumanCrew;
use orbit_types::task::{
    DecisionRequest, PilotAdmissionOperatorRecord, Task, TaskHistoryEntry, TaskStatus, TaskType,
};
use orbit_types::tool::ToolSessionContext;
use orbit_types::workflow::JobRun;
use orbit_types::workspace::WorkspacePublicationBinding;
use serde_json::{Map, Value};

use super::exec::{self, App, MutationAudit, Operation};
use crate::Core;
use crate::application::resident::ResidentActionTicket;
use crate::application::task::{
    DecisionChannelInfo, ExternalMergeSettlement, PostDeployOperatorRecord, ProposedTaskInput,
    ScreenedTaskWrite, TaskAddParams, TaskListFilter, TaskListQuery, TaskPage, TaskRow,
    TaskUpdateAuthority, TaskUpdateParams, TransientInfraBlockedTask,
};
use crate::application::task_bundle::migration::{
    ExportOutcome, ExportSelection, ImportConflictPolicy, ImportOutcome, ReindexOutcome,
};
use crate::application::task_bundle::publication::{
    AttachmentPolicy, PublicationInspection, PublicationPublishOutcome, PublicationRestoreOutcome,
};
use crate::runtime::InfraBlockedTask;

/// One task command.
pub enum Command<'a> {
    /// A verb of the task operation registry.
    Operation(TaskVerb, Input<'a>),
    /// A verb of the decision operation registry.
    Decision(DecisionVerb, Input<'a>),
    /// Archive a task in any status.
    Archive(Archive),
    /// Every task carrying the tags, each with its history.
    Flow {
        /// Tags a task must carry; none selects every task.
        tags: Vec<String>,
        /// Only tasks of this type.
        task_type: Option<TaskType>,
    },
    /// The infra-blocked tasks, requeueing the cleared ones on `confirm`.
    RecheckBlocked {
        /// Return the cleared tasks to backlog.
        confirm: bool,
    },
    /// Settle tasks whose delivery pull request merged outside Orbit.
    SettleMerged(SettleMerged),
    /// Pack task bundles into an archive.
    Export {
        /// Task-registry workspace to export from; the bound one when absent.
        task_workspace: Option<String>,
        /// Which tasks to export.
        selection: ExportSelection,
        /// The archive to write.
        output: PathBuf,
    },
    /// Unpack an archive of task bundles.
    Import {
        /// The archive to read.
        archive: PathBuf,
        /// Task-registry workspace to import into; the bound one when absent.
        task_workspace: Option<String>,
        /// What to do with a task id that already exists.
        on_conflict: ImportConflictPolicy,
    },
    /// Rebuild the task index from the bundles on disk.
    Reindex {
        /// Task-registry workspace to reindex; the bound one when absent.
        task_workspace: Option<String>,
    },
    /// The task-publication and recovery workflow.
    Publication(Publication),
    /// A record the dashboard's operator writes that has no registry verb.
    Record(DashboardWrite),
    /// A read model the dashboard takes.
    Read(Read),
}

impl Command<'static> {
    /// The command of one dashboard write. A write that is a registry verb
    /// is that verb's command, so it ends in the arm the command line and
    /// the tool reach.
    pub fn dashboard(write: DashboardWrite) -> Self {
        let input = |write| Input {
            request: Request::Dashboard(Box::new(write)),
        };
        match &write.action {
            DashboardAction::Add { .. } => Self::Operation(TaskVerb::Add, input(write)),
            DashboardAction::Update { .. }
            | DashboardAction::Comment { .. }
            | DashboardAction::Approve { .. } => Self::Operation(TaskVerb::Update, input(write)),
            DashboardAction::Reject { .. } => Self::Operation(TaskVerb::Reject, input(write)),
            DashboardAction::Requeue { .. } => Self::Operation(TaskVerb::Requeue, input(write)),
            DashboardAction::AnswerDecision { .. } => {
                Self::Decision(DecisionVerb::Answer, input(write))
            }
            DashboardAction::Archive { .. }
            | DashboardAction::SettleMerged { .. }
            | DashboardAction::PilotAdmission { .. }
            | DashboardAction::PostDeploy { .. }
            | DashboardAction::Propose(_) => Self::Record(write),
        }
    }
}

/// What a registry verb is called with.
pub struct Input<'a> {
    request: Request<'a>,
}

/// The form a registry verb's request arrived in.
enum Request<'a> {
    /// Fields under the verb's spec, from a command line or a server.
    Fields(Value),
    /// The call the generic tool entry admitted.
    Tool(Value, Box<ToolCall<'a>>),
    /// A typed write from the dashboard.
    Dashboard(Box<DashboardWrite>),
}

impl Input<'static> {
    /// A request's fields, named as the verb's spec names them.
    pub fn new(fields: Map<String, Value>) -> Self {
        Self {
            request: Request::Fields(Value::Object(fields)),
        }
    }
}

impl<'a> Input<'a> {
    /// The call the generic tool entry admitted: its input and its caller.
    pub(crate) fn from_tool_entry(input: Value, call: ToolCall<'a>) -> Self {
        Self {
            request: Request::Tool(input, Box::new(call)),
        }
    }

    /// The fields a request names, for the audit row's target.
    fn fields(&self) -> &Value {
        match &self.request {
            Request::Fields(fields) | Request::Tool(fields, _) => fields,
            Request::Dashboard(_) => &Value::Null,
        }
    }
}

/// Everything the tool entry knows about *who* is making a call.
///
/// Grouped rather than passed as four more parameters because the flows
/// consume different subsets of it: attribution for a persisted record, the
/// reservation owner for a lock write, and the session itself for the one
/// operation whose decision depends on the caller.
pub(crate) struct ToolCaller<'a> {
    pub(crate) session_context: &'a ToolSessionContext,
    pub(crate) agent: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) reservation_owner: Option<ReservationOwnerContext>,
}

/// A tool call as the generic tool entry admitted it.
pub(crate) struct ToolCall<'a> {
    pub(crate) caller: ToolCaller<'a>,
    /// The caller holds operator capability and names no agent.
    pub(crate) operator: bool,
    /// The authenticated resident cycle's action key, when one made the call.
    pub(crate) resident_action_key: Option<&'a str>,
    pub(crate) resident_ticket: Option<&'a ResidentActionTicket>,
}

/// Archive one task.
pub struct Archive {
    /// The task.
    pub id: String,
    /// Archive even if a live local run owns the task.
    pub force: bool,
}

/// Which merged deliveries to settle.
pub enum SettleMerged {
    /// Every task whose delivery merged recently.
    All,
    /// One task, reading each of its pull requests.
    Task {
        /// The task.
        id: String,
    },
    /// One task whose refused follower handoff was rescued by hand.
    RecoveredClaim {
        /// The task.
        id: String,
        /// The owner-recovered follower claim.
        claim: String,
        /// The merged delivery pull request.
        pull_request: u64,
        /// The original published head commit the operator attests.
        published_head: String,
    },
}

/// One command of the task-publication workflow.
pub enum Publication {
    /// Publish the bound workspace's task snapshot.
    Publish {
        /// What to do with attachments.
        policy: AttachmentPolicy,
    },
    /// The bound publication's recorded and remote state.
    Status,
    /// Read a published snapshot.
    Inspect(PublicationSource),
    /// Restore a published snapshot into this workspace's task store.
    Restore {
        /// The snapshot to restore.
        source: PublicationSource,
        /// Accept a destination that already holds the identical snapshot.
        allow_identical_retry: bool,
    },
}

/// The publication a consumer names.
#[derive(Clone)]
pub struct PublicationSource {
    /// The workspace the snapshot was published from.
    pub workspace_id: String,
    /// The source repository's remote.
    pub source_repository_fingerprint: String,
    /// The publication.
    pub publication_id: String,
    /// The machine holding publication authority.
    pub authority_machine_id: String,
    /// The publication remote.
    pub remote: String,
    /// The publication branch.
    pub branch: String,
    /// One commit of the publication; its tip when absent.
    pub commit: Option<String>,
}

/// One write from the dashboard, and what its mutation row records.
pub struct DashboardWrite {
    /// The mutation row's verb.
    pub verb: &'static str,
    /// The record the request names, when it names one.
    pub target_id: Option<String>,
    /// The forwarding caller, when the request came through another host.
    pub caller: Option<String>,
    /// The role recorded for the write; resolved from `model` when absent.
    pub role: Option<String>,
    /// The model the request attributes the write to.
    pub model: Option<String>,
    /// The mutation row's detail.
    pub detail: Value,
    /// What to write.
    pub action: DashboardAction,
}

/// What one dashboard write does.
pub enum DashboardAction {
    /// Create a task.
    Add {
        /// The task's fields.
        params: Box<TaskAddParams>,
        /// Store context selectors whose targets do not exist yet.
        allow_missing_context: bool,
    },
    /// Apply a partial update.
    Update {
        /// The task.
        id: String,
        /// The fields to change.
        params: Box<TaskUpdateParams>,
        /// Store context selectors whose targets do not exist yet.
        allow_missing_context: bool,
        /// Whether a status change follows the lifecycle table.
        authority: TaskUpdateAuthority,
        /// The note recorded with a status change.
        status_note: Option<String>,
    },
    /// Append a human comment.
    Comment {
        /// The task.
        id: String,
        /// The comment.
        message: String,
        /// The human author.
        author: String,
    },
    /// Approve a task in review.
    Approve {
        /// The task.
        id: String,
        /// The approval note.
        note: Option<String>,
        /// A comment recorded with the approval.
        comment: Option<String>,
    },
    /// Reject a task.
    Reject {
        /// The task.
        id: String,
        /// The rejection note.
        note: String,
        /// A comment recorded with the rejection.
        comment: Option<String>,
    },
    /// Return a blocked task to backlog, or discard its candidate.
    Requeue {
        /// The task.
        id: String,
        /// Why; recorded with the decision.
        reason: String,
        /// Discard the published candidate instead of resuming it.
        discard_candidate: bool,
        /// The actor recorded in task history.
        actor: &'static str,
    },
    /// Answer a decision request as a human.
    AnswerDecision {
        /// The decision request.
        id: String,
        /// The chosen answer.
        answer: String,
        /// A note recorded with the answer.
        note: Option<String>,
        /// The human author.
        author: String,
    },
    /// Archive a task in any status.
    Archive {
        /// The task.
        id: String,
        /// Archive even if a live local run owns the task.
        force: bool,
    },
    /// Settle one task whose delivery merged outside Orbit.
    SettleMerged {
        /// The task.
        id: String,
        /// The actor recorded in task history.
        actor: &'static str,
    },
    /// Record the operator's pilot admission verdict.
    PilotAdmission {
        /// The task.
        id: String,
        /// The verdict and its evidence.
        record: PilotAdmissionOperatorRecord,
        /// The actor recorded in task history.
        actor: &'static str,
    },
    /// Record a post-deploy criterion's measurement.
    PostDeploy {
        /// The task.
        id: String,
        /// The criterion, its outcome and the evidence.
        record: PostDeployOperatorRecord,
        /// The actor recorded in task history.
        actor: &'static str,
    },
    /// Create a proposed task from a frozen submission.
    Propose(Box<ProposedTaskInput>),
}

/// One read model of the dashboard.
pub enum Read {
    /// Task pages of the bound workspace.
    Pages {
        /// Names the pages. It receives the workspace's id, which a
        /// continuation cursor is bound to.
        select: PageSelect,
        /// A task whose dependency statuses the page holding it answers.
        detail: Option<String>,
    },
    /// One task's row, with the statuses of its dependencies.
    Row {
        /// The task.
        id: String,
    },
    /// The live lock and reservation snapshot.
    Locks,
    /// The status distribution of tasks per complexity.
    CompletionByComplexity,
}

/// Names the task pages to read once the workspace's id is known.
pub struct PageSelect(Box<SelectPages>);

/// Names the pages of the workspace whose id it is called with.
type SelectPages = dyn FnOnce(&str) -> Result<PageSelection, OrbitError> + Send;

impl PageSelect {
    /// Select pages with `select`, called with the bound workspace's id.
    pub fn new(
        select: impl FnOnce(&str) -> Result<PageSelection, OrbitError> + Send + 'static,
    ) -> Self {
        Self(Box::new(select))
    }
}

/// The task pages one read asks for.
pub enum PageSelection {
    /// One page.
    One(TaskListQuery),
    /// One page per filter and limit, selected in one batch under `query`.
    Sections {
        /// The fields every section shares.
        query: TaskListQuery,
        /// Each section's filter and limit.
        sections: Vec<(TaskListFilter, usize)>,
    },
}

/// What a command answers.
pub enum Outcome {
    /// A registry verb's answer, or another JSON document.
    Answer(Value),
    /// A tool call's answer and the id it persisted, for the tool entry.
    ToolAnswer(Value, Option<String>),
    /// A task write from the command line.
    Written(Box<Written>),
    /// A lock reservation's answer; a refused one exits non-zero.
    Reservation(Value),
    /// A task's full record, with what the command line shows beside it.
    Shown(Box<Shown>),
    /// Tasks with their histories.
    Flow(Vec<(Task, Vec<TaskHistoryEntry>)>),
    /// The infra-blocked tasks and the ones requeued.
    Rechecked(Rechecked),
    /// The settled tasks.
    Settled(Vec<ExternalMergeSettlement>),
    /// What an export packed.
    Exported(ExportOutcome),
    /// What an import unpacked.
    Imported(ImportOutcome),
    /// What a reindex rebuilt.
    Reindexed(ReindexOutcome),
    /// A published snapshot and its binding.
    Published(Box<(WorkspacePublicationBinding, PublicationPublishOutcome)>),
    /// A publication binding and, once it has a snapshot, the remote's.
    PublicationStatus(Box<(WorkspacePublicationBinding, Option<PublicationInspection>)>),
    /// A published snapshot.
    Inspected(Box<PublicationInspection>),
    /// What a restore wrote.
    Restored(PublicationRestoreOutcome),
    /// A decision request after a write.
    DecisionRequest(Box<DecisionRequest>),
    /// A decision channel after a write.
    Channel(DecisionChannelInfo),
    /// This host's decision channels.
    Channels(Vec<DecisionChannelInfo>),
    /// A dashboard write that answers the task.
    Screened(Box<ScreenedWrite>),
    /// A task after a record was written on it.
    Task(Box<Task>),
    /// The id of a created task.
    Created(String),
    /// A write with nothing to answer.
    Done,
    /// Task pages.
    Pages(TaskPages),
    /// One task's row.
    Row(Box<RowRead>),
    /// Task counts per complexity.
    Completion(Vec<TaskCompletionByComplexity>),
}

/// A command-line task write: the stored task's full record and the write's
/// notices.
pub struct Written {
    /// The full-record projection of the stored task.
    pub document: Value,
    /// The stored task.
    pub task: Task,
    /// The write's warnings.
    pub warnings: Vec<String>,
    /// Context selectors stored without a verified target.
    pub context_files_unverified: Vec<String>,
}

/// A task's full record as the command line shows it.
pub struct Shown {
    /// The tool's answer.
    pub answer: Value,
    /// The registered workspace the record was read from: its id and name.
    pub workspace: Option<(String, String)>,
    /// The status of each relation target that has one.
    pub relation_statuses: BTreeMap<String, TaskStatus>,
    /// The task's run, when it is active or in review and names one. `None`
    /// inside means the run could not be read.
    pub run: Option<Option<JobRun>>,
}

/// The infra-blocked tasks of one recheck.
pub struct Rechecked {
    /// Tasks blocked by a missing provider launcher.
    pub found: Vec<InfraBlockedTask>,
    /// Tasks blocked by transient infrastructure.
    pub transient: Vec<TransientInfraBlockedTask>,
    /// The launcher-blocked tasks this recheck requeued.
    pub requeued: Vec<InfraBlockedTask>,
    /// The transient-blocked tasks this recheck requeued.
    pub transient_requeued: Vec<TransientInfraBlockedTask>,
}

/// A dashboard write that answers the task it wrote.
pub struct ScreenedWrite {
    /// The write.
    pub written: ScreenedTaskWrite,
    /// The dashboard's full record of the task. The write stands when the
    /// record cannot be rendered.
    pub document: Result<Value, OrbitError>,
    /// The human crew the task is assigned to, when it is one.
    pub human_crew: Option<HumanCrew>,
}

/// Task pages of one workspace.
pub struct TaskPages {
    /// The workspace's id.
    pub workspace_id: String,
    /// The pages, in the order they were selected. A failed read is
    /// answered beside the workspace, which was resolved.
    pub pages: Result<Vec<PageRead>, OrbitError>,
}

/// One task page, with what its rows show beside the task.
pub struct PageRead {
    /// The page.
    pub page: TaskPage,
    /// Delivery attention per task id.
    pub delivery_attention: BTreeMap<String, Value>,
    /// The local run branch per task id.
    pub run_branches: BTreeMap<String, String>,
    /// The status of each dependency of the read's `detail` task, when this
    /// page holds it.
    pub detail_dependency_statuses: Option<Result<BTreeMap<String, TaskStatus>, OrbitError>>,
}

/// One task's row.
pub struct RowRead {
    /// The row.
    pub row: TaskRow,
    /// The status of each of the task's dependencies. A failed lookup is
    /// answered beside the row, which was read.
    pub dependency_statuses: Result<BTreeMap<String, TaskStatus>, OrbitError>,
}

macro_rules! outcome_accessors {
    ($($(#[$doc:meta])* $method:ident => $variant:ident($inner:ty)),* $(,)?) => {
        impl Outcome {
            $(
                $(#[$doc])*
                pub fn $method(self) -> Result<$inner, crate::Error> {
                    match self {
                        Self::$variant(inner) => Ok(inner),
                        _ => Err(unexpected_outcome(stringify!($variant))),
                    }
                }
            )*
        }
    };
}

outcome_accessors! {
    /// The JSON answer.
    into_answer => Answer(Value),
    /// The command-line task write.
    into_written => Written(Box<Written>),
    /// The lock reservation's answer.
    into_reservation => Reservation(Value),
    /// The task record the command line shows.
    into_shown => Shown(Box<Shown>),
    /// The tasks with their histories.
    into_flow => Flow(Vec<(Task, Vec<TaskHistoryEntry>)>),
    /// The rechecked infra-blocked tasks.
    into_rechecked => Rechecked(Rechecked),
    /// The settled tasks.
    into_settled => Settled(Vec<ExternalMergeSettlement>),
    /// The export.
    into_exported => Exported(ExportOutcome),
    /// The import.
    into_imported => Imported(ImportOutcome),
    /// The reindex.
    into_reindexed => Reindexed(ReindexOutcome),
    /// The published snapshot and its binding.
    into_published => Published(Box<(WorkspacePublicationBinding, PublicationPublishOutcome)>),
    /// The publication binding and the remote's snapshot.
    into_publication_status =>
        PublicationStatus(Box<(WorkspacePublicationBinding, Option<PublicationInspection>)>),
    /// The published snapshot.
    into_inspected => Inspected(Box<PublicationInspection>),
    /// The restore.
    into_restored => Restored(PublicationRestoreOutcome),
    /// The decision request.
    into_decision_request => DecisionRequest(Box<DecisionRequest>),
    /// The decision channel.
    into_channel => Channel(DecisionChannelInfo),
    /// The decision channels.
    into_channels => Channels(Vec<DecisionChannelInfo>),
    /// The dashboard write and its record.
    into_screened => Screened(Box<ScreenedWrite>),
    /// The task.
    into_task => Task(Box<Task>),
    /// The created task's id.
    into_created => Created(String),
    /// The task pages.
    into_pages => Pages(TaskPages),
    /// The task row.
    into_row => Row(Box<RowRead>),
    /// The task counts per complexity.
    into_completion => Completion(Vec<TaskCompletionByComplexity>),
}

impl Outcome {
    /// A tool call's answer and the id it persisted.
    pub(crate) fn into_tool_answer(self) -> Result<(Value, Option<String>), crate::Error> {
        match self {
            Self::ToolAnswer(value, persisted) => Ok((value, persisted)),
            _ => Err(unexpected_outcome("ToolAnswer")),
        }
    }
}

/// A command answered another outcome than the one its surface renders.
fn unexpected_outcome(expected: &str) -> crate::Error {
    OrbitError::Execution(format!(
        "task command answered an outcome other than {expected}"
    ))
}

/// Why a task command failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The flow that ran the command failed.
    #[error(transparent)]
    Flow(#[from] OrbitError),
}

impl From<Error> for crate::Error {
    fn from(error: Error) -> Self {
        match error {
            Error::Flow(error) => error,
        }
    }
}

/// Run one task command: authorize it, audit it, run it.
pub fn exec<'a>(core: &Core, command: Command<'a>) -> Result<Outcome, crate::Error> {
    exec::run::<TaskApp<'a>>(core, command)
}

struct TaskApp<'a>(PhantomData<&'a ()>);

impl<'a> App for TaskApp<'a> {
    type Command = Command<'a>;
    type Outcome = Outcome;
    type Error = Error;

    fn operation(command: &Command<'a>) -> Operation {
        surface::operation(command)
    }

    fn run(core: &Core, command: Command<'a>) -> Result<Outcome, Error> {
        let runtime = core.runtime();
        let outcome = match command {
            Command::Operation(verb, input) => match input.request {
                Request::Tool(fields, call) => operation::dispatch(runtime, *call, verb, fields)
                    .map(|(value, persisted)| Outcome::ToolAnswer(value, persisted)),
                Request::Fields(fields) => surface::operation_fields(core, verb, fields),
                Request::Dashboard(write) => web::write(runtime, *write),
            },
            Command::Decision(verb, input) => match input.request {
                Request::Tool(fields, call) => decision::dispatch(
                    runtime,
                    verb,
                    fields,
                    call.caller.session_context,
                    call.caller.agent.as_deref(),
                    call.caller.model.as_deref(),
                )
                .map(|value| Outcome::ToolAnswer(value, None)),
                Request::Fields(fields) => surface::decision_fields(core, verb, fields),
                Request::Dashboard(write) => web::write(runtime, *write),
            },
            Command::Archive(archive) => surface::archive(runtime, archive),
            Command::Flow { tags, task_type } => surface::flow(runtime, &tags, task_type),
            Command::RecheckBlocked { confirm } => surface::recheck_blocked(runtime, confirm),
            Command::SettleMerged(settle) => surface::settle_merged(runtime, settle),
            Command::Export {
                task_workspace,
                selection,
                output,
            } => runtime
                .export_tasks(task_workspace.as_deref(), selection, &output)
                .map(Outcome::Exported),
            Command::Import {
                archive,
                task_workspace,
                on_conflict,
            } => runtime
                .import_tasks(&archive, task_workspace.as_deref(), on_conflict)
                .map(Outcome::Imported),
            Command::Reindex { task_workspace } => runtime
                .reindex_tasks(task_workspace.as_deref())
                .map(Outcome::Reindexed),
            Command::Publication(command) => publication::run(runtime, command),
            Command::Record(write) => web::write(runtime, write),
            Command::Read(read) => web::read(runtime, read),
        };
        outcome.map_err(Error::Flow)
    }

    fn exit_code(outcome: &Outcome) -> i32 {
        match outcome {
            Outcome::Reservation(value) if value["reserved"].as_bool() != Some(true) => 3,
            _ => 0,
        }
    }

    fn mutation_audit(command: &Command<'a>) -> Option<MutationAudit> {
        let write = match command {
            Command::Operation(_, input) | Command::Decision(_, input) => match &input.request {
                Request::Dashboard(write) => write.as_ref(),
                Request::Fields(_) | Request::Tool(..) => return None,
            },
            Command::Record(write) => write,
            _ => return None,
        };
        Some(MutationAudit {
            verb: write.verb,
            target_id: write.target_id.clone(),
            caller: write.caller.clone(),
            role: write.role.clone(),
            model: write.model.clone(),
            detail: write.detail.clone(),
        })
    }

    fn written_id(outcome: &Outcome) -> Option<String> {
        match outcome {
            Outcome::Screened(write) => Some(write.written.task.id.clone()),
            Outcome::Created(id) => Some(id.clone()),
            Outcome::DecisionRequest(request) => Some(request.task_id.clone()),
            _ => None,
        }
    }
}
