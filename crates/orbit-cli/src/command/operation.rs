//! Compiler-enforced meaning for every top-level CLI command.
//!
//! ADR-0209: command behavior is declared as operation data. The exhaustive
//! [`Commands::operation`] match is the only top-level declaration site for
//! dispatch, runtime bootstrap, audit metadata, JSON error output, and hook
//! error suppression. Adding a [`Commands`] variant therefore requires one
//! new operation arm and the compiler rejects an incomplete registry.

use std::path::Path;

use orbit_core::{ActorIdentity, OrbitError, OrbitRuntime};
use orbit_types::identity::{
    normalize_agent_family_for_model, normalize_optional_attribution_label,
};
use serde_json::Value;

use super::{CommandOut, CommandOutput, Commands, Execute};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandMeta {
    pub command: String,
    pub subcommand: Option<String>,
    pub tool_name: Option<String>,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub role: String,
    pub arguments_json: Option<String>,
    pub job_run_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeNeed {
    Required,
    /// Read an existing workspace without stale-run reconciliation on open.
    ReadOnly,
    Forbidden,
    /// Bind the workspace that owns this task ID rather than the one the cwd
    /// or `--workspace` walk would pick [ORB-10797] [ORB-10961].
    ///
    /// Task IDs are a machine-global primary key, so `task show` — including
    /// `orbit tool run orbit.task.show` — is the one verb whose target is
    /// addressable without knowing its workspace. A `--workspace` selector
    /// still wins and still filters: the bootstrap binds that workspace, and a
    /// task owned elsewhere is simply not found.
    TaskOwner {
        task_id: String,
    },
}

pub struct DispatchContext<'a> {
    runtime: Option<&'a OrbitRuntime>,
    root_override: Option<&'a Path>,
    /// The global `--workspace` selector, for the runtime-forbidden commands
    /// that resolve their own workspaces instead of being handed one runtime.
    workspace_selector: Option<&'a str>,
}

impl<'a> DispatchContext<'a> {
    pub fn with_runtime(
        runtime: &'a OrbitRuntime,
        root_override: Option<&'a Path>,
        workspace_selector: Option<&'a str>,
    ) -> Self {
        Self {
            runtime: Some(runtime),
            root_override,
            workspace_selector,
        }
    }

    pub fn without_runtime(
        root_override: Option<&'a Path>,
        workspace_selector: Option<&'a str>,
    ) -> Self {
        Self {
            runtime: None,
            root_override,
            workspace_selector,
        }
    }

    fn runtime(&self) -> Result<&'a OrbitRuntime, OrbitError> {
        self.runtime.ok_or_else(|| {
            OrbitError::Execution(
                "command operation required a runtime but dispatch did not provide one".to_string(),
            )
        })
    }
}

pub type CommandDispatch = for<'a> fn(Commands, DispatchContext<'a>) -> CommandOut;

pub struct CommandOperation {
    pub runtime_need: RuntimeNeed,
    pub audit_meta: Option<CommandMeta>,
    pub json_error_preference: Option<bool>,
    pub suppress_errors: bool,
    pub dispatch: CommandDispatch,
    /// The governed operation this invocation performs, when it performs one
    /// [ORB-10453].
    ///
    /// An arm sets this to name *which* operation is being invoked; it never
    /// names a capability. The requirement lives in
    /// `orbit_common::governance::authorization::GOVERNED_OPERATIONS` and the decision is
    /// made once, in `main`, before dispatch.
    ///
    /// Commands whose destruction is flag-gated (`--confirm`) set it only for
    /// the destructive invocation, so the read-only report stays reachable.
    pub governed: Option<GovernedCommand>,
}

/// A `<command> <subcommand>` pair to authorize before dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GovernedCommand {
    pub command: &'static str,
    pub subcommand: &'static str,
}

impl CommandOperation {
    fn new(
        runtime_need: RuntimeNeed,
        audit_meta: Option<CommandMeta>,
        json_error_preference: Option<bool>,
        suppress_errors: bool,
        dispatch: CommandDispatch,
    ) -> Self {
        Self {
            runtime_need,
            audit_meta,
            json_error_preference,
            suppress_errors,
            dispatch,
            governed: None,
        }
    }

    /// Mark this invocation as performing a governed operation.
    ///
    /// `when` is the destructiveness predicate: `gc worktrees` reports without
    /// it and reaps with it, and only the second is governed.
    fn governed_when(
        mut self,
        when: bool,
        command: &'static str,
        subcommand: &'static str,
    ) -> Self {
        if when {
            self.governed = Some(GovernedCommand {
                command,
                subcommand,
            });
        }
        self
    }

    /// Apply the process actor resolved by the runtime bootstrap to the CLI
    /// guard's audit row. Tool dispatch may replace this row with its own
    /// more specific audited invocation, but pre-dispatch failures and all
    /// direct commands must use the same actor identity as the runtime.
    pub fn attribute_to(mut self, actor: &ActorIdentity) -> Self {
        if let Some(meta) = self.audit_meta.as_mut() {
            meta.role = actor.audit_role().to_string();
        }
        self
    }
}

macro_rules! runtime_dispatch {
    ($variant:ident) => {{
        |command, context| match command {
            Commands::$variant(command) => command.execute(context.runtime()?),
            _ => dispatch_mismatch(stringify!($variant)),
        }
    }};
}

macro_rules! boxed_runtime_dispatch {
    ($variant:ident) => {{
        |command, context| match command {
            Commands::$variant(command) => (*command).execute(context.runtime()?),
            _ => dispatch_mismatch(stringify!($variant)),
        }
    }};
}

fn dispatch_mismatch(variant: &str) -> CommandOut {
    Err(OrbitError::Execution(format!(
        "command operation dispatch invariant violated for {variant}"
    )))
}

fn admin_meta(
    command: &str,
    subcommand: Option<&str>,
    target_type: Option<&str>,
    target_id: Option<&str>,
) -> CommandMeta {
    CommandMeta {
        command: command.to_string(),
        subcommand: subcommand.map(String::from),
        tool_name: None,
        target_type: target_type.map(String::from),
        target_id: target_id.map(String::from),
        role: "admin".to_string(),
        arguments_json: None,
        job_run_id: None,
    }
}

impl Commands {
    /// Resolve all cross-cutting behavior for this command from one exhaustive
    /// declaration. Do not add a wildcard arm: exhaustiveness is the guardrail
    /// that keeps new CLI commands from silently inheriting policy defaults.
    pub fn operation(&self) -> CommandOperation {
        match self {
            Commands::Init(_) => CommandOperation::new(
                RuntimeNeed::Forbidden,
                Some(admin_meta("init", None, Some("config"), None)),
                None,
                false,
                dispatch_init,
            ),
            Commands::Workspace(command) => {
                use super::workspace::WorkspacePublicationSubcommand;
                use super::workspace::WorkspaceSourceRemoteSubcommand;
                use super::workspace::WorkspaceSubcommand;
                let (subcommand, runtime_need, governed) = match &command.command {
                    WorkspaceSubcommand::Init(_) => ("init", RuntimeNeed::Forbidden, false),
                    WorkspaceSubcommand::Sync(_) => ("sync", RuntimeNeed::Forbidden, false),
                    WorkspaceSubcommand::List(_) => ("list", RuntimeNeed::Required, false),
                    WorkspaceSubcommand::Show(_) => ("show", RuntimeNeed::Required, false),
                    WorkspaceSubcommand::SourceRemote(command) => match &command.command {
                        WorkspaceSourceRemoteSubcommand::Show(_) => {
                            ("source-remote-show", RuntimeNeed::Required, false)
                        }
                        WorkspaceSourceRemoteSubcommand::Rebind(_) => {
                            ("source-remote-rebind", RuntimeNeed::Required, false)
                        }
                    },
                    WorkspaceSubcommand::Role(_) => ("role", RuntimeNeed::Required, false),
                    WorkspaceSubcommand::Publication(command) => match &command.command {
                        WorkspacePublicationSubcommand::Bind(_) => {
                            ("publication-bind", RuntimeNeed::Required, false)
                        }
                        WorkspacePublicationSubcommand::Show(_) => {
                            ("publication-show", RuntimeNeed::Required, false)
                        }
                        WorkspacePublicationSubcommand::Rebind(_) => {
                            ("publication-rebind", RuntimeNeed::Required, true)
                        }
                        WorkspacePublicationSubcommand::Remove(args) => {
                            ("publication-remove", RuntimeNeed::Required, args.confirm)
                        }
                    },
                    WorkspaceSubcommand::Remove(_) => ("remove", RuntimeNeed::Required, true),
                    WorkspaceSubcommand::Teardown(args) => {
                        ("teardown", RuntimeNeed::Required, args.confirm)
                    }
                };
                CommandOperation::new(
                    runtime_need,
                    Some(admin_meta(
                        "workspace",
                        Some(subcommand),
                        Some("workspace"),
                        None,
                    )),
                    None,
                    false,
                    dispatch_workspace,
                )
                .governed_when(governed, "workspace", subcommand)
            }
            Commands::Host(command) => {
                use super::host::HostSubcommand;
                let (subcommand, runtime_need, json_output) = match &command.command {
                    HostSubcommand::Show(args) => ("show", RuntimeNeed::Forbidden, args.json),
                    HostSubcommand::Rename(_) => ("rename", RuntimeNeed::Required, false),
                };
                CommandOperation::new(
                    runtime_need,
                    Some(admin_meta("host", Some(subcommand), Some("host"), None)),
                    json_output.then_some(true),
                    false,
                    dispatch_host,
                )
            }
            Commands::Config(command) => {
                use super::config::ConfigSubcommand;
                let subcommand = match &command.command {
                    ConfigSubcommand::Show(_) => "show",
                    ConfigSubcommand::Get(_) => "get",
                    ConfigSubcommand::Set(_) => "set",
                    ConfigSubcommand::Keys(_) => "keys",
                    ConfigSubcommand::Path(_) => "path",
                };
                CommandOperation::new(
                    RuntimeNeed::Required,
                    Some(admin_meta("config", Some(subcommand), Some("config"), None)),
                    None,
                    false,
                    runtime_dispatch!(Config),
                )
            }
            Commands::Semantic(command) => {
                use super::semantic::SemanticSubcommand;
                let subcommand = match &command.command {
                    SemanticSubcommand::Install(_) => "install",
                    SemanticSubcommand::Uninstall(_) => "uninstall",
                    SemanticSubcommand::Stats(_) => "stats",
                    SemanticSubcommand::Index(_) => "index",
                };
                CommandOperation::new(
                    RuntimeNeed::Required,
                    Some(admin_meta(
                        "semantic",
                        Some(subcommand),
                        Some("semantic_index"),
                        None,
                    )),
                    None,
                    false,
                    runtime_dispatch!(Semantic),
                )
                .governed_when(subcommand == "uninstall", "semantic", subcommand)
            }
            Commands::Migrate(command) => CommandOperation::new(
                if command.confirm {
                    RuntimeNeed::Required
                } else {
                    RuntimeNeed::Forbidden
                },
                Some(admin_meta("migrate", None, Some("workspace"), None)),
                None,
                false,
                dispatch_migrate,
            ),
            Commands::Update(_) => CommandOperation::new(
                // Forbidden, not merely unused: opening a workspace here would
                // auto-apply *this* binary's migrations, when the whole point
                // is to let the replacement binary apply its own.
                RuntimeNeed::Forbidden,
                Some(admin_meta("update", None, Some("installation"), None)),
                None,
                false,
                dispatch_update,
            ),
            Commands::Run(command) => {
                use super::run::RunSubcommand;
                let (subcommand, target_type, target_id, runtime_need) = match &command.command {
                    RunSubcommand::Agent(_) => (
                        "agent",
                        Some("workflow"),
                        Some("agent_invoke"),
                        RuntimeNeed::Required,
                    ),
                    RunSubcommand::Auto(_) => (
                        "auto",
                        Some("workflow"),
                        Some("auto"),
                        RuntimeNeed::Required,
                    ),
                    RunSubcommand::Ship(_) => (
                        "ship",
                        Some("workflow"),
                        Some("ship"),
                        RuntimeNeed::Required,
                    ),
                    RunSubcommand::ShipLocal(_) => (
                        "ship-local",
                        Some("workflow"),
                        Some("ship-local"),
                        RuntimeNeed::Required,
                    ),
                    RunSubcommand::ShipSweep(_) => (
                        "ship-sweep",
                        Some("workflow"),
                        Some("ship-sweep"),
                        RuntimeNeed::Forbidden,
                    ),
                    RunSubcommand::Triage(_) => (
                        "triage",
                        Some("workflow"),
                        Some("triage"),
                        RuntimeNeed::Required,
                    ),
                    RunSubcommand::Readiness(_) => (
                        "readiness",
                        Some("workspace"),
                        Some("auto_readiness"),
                        RuntimeNeed::ReadOnly,
                    ),
                    RunSubcommand::History(args) => (
                        "history",
                        Some("job_run"),
                        args.job_id.as_deref(),
                        RuntimeNeed::Required,
                    ),
                    RunSubcommand::Show(args) => (
                        "show",
                        Some("job_run"),
                        args.run_id.as_deref(),
                        RuntimeNeed::Required,
                    ),
                    RunSubcommand::Logs(args) => (
                        "logs",
                        Some("job_run"),
                        args.run_id.as_deref(),
                        RuntimeNeed::Required,
                    ),
                    RunSubcommand::Events(args) => (
                        "events",
                        Some("job_run"),
                        args.run_id.as_deref(),
                        RuntimeNeed::Required,
                    ),
                    RunSubcommand::Trace(args) => (
                        "trace",
                        Some("job_run"),
                        args.run_id.as_deref(),
                        RuntimeNeed::Required,
                    ),
                    RunSubcommand::Cancel(args) => (
                        "cancel",
                        Some("job_run"),
                        Some(args.run_id.as_str()),
                        RuntimeNeed::Required,
                    ),
                    RunSubcommand::Concurrency(args) => (
                        "concurrency",
                        Some("job_run"),
                        Some(args.run_id.as_str()),
                        RuntimeNeed::Required,
                    ),
                    RunSubcommand::Job(args) => (
                        "job",
                        Some("job"),
                        Some(args.job_id.as_str()),
                        RuntimeNeed::Required,
                    ),
                };
                CommandOperation::new(
                    runtime_need,
                    Some(admin_meta("run", Some(subcommand), target_type, target_id)),
                    None,
                    false,
                    dispatch_run,
                )
            }
            Commands::Gc(command) => {
                use super::gc::GcTarget;
                let (target, reaps) = match &command.target {
                    GcTarget::Worktrees(args) => ("worktrees", args.confirm),
                };
                CommandOperation::new(
                    RuntimeNeed::Required,
                    Some(admin_meta(
                        "gc",
                        Some(target),
                        Some("garbage"),
                        Some(target),
                    )),
                    None,
                    false,
                    runtime_dispatch!(Gc),
                )
                .governed_when(reaps, "gc", target)
            }
            Commands::Clock(command) => {
                use super::clock::ClockSubcommand;
                let subcommand = match &command.command {
                    ClockSubcommand::Status => "status",
                    ClockSubcommand::Pause => "pause",
                    ClockSubcommand::Enable => "enable",
                    ClockSubcommand::Set { .. } => "set",
                    ClockSubcommand::Tick(_) => "tick",
                };
                CommandOperation::new(
                    RuntimeNeed::Forbidden,
                    Some(admin_meta(
                        "clock",
                        Some(subcommand),
                        Some("clock"),
                        Some("host"),
                    )),
                    None,
                    false,
                    dispatch_clock,
                )
            }
            Commands::Sweep(_) => CommandOperation::new(
                RuntimeNeed::Forbidden,
                Some(admin_meta(
                    "clock",
                    Some("tick"),
                    Some("clock"),
                    Some("host"),
                )),
                None,
                false,
                dispatch_sweep,
            ),
            Commands::Routine(command) => {
                use super::routine::RoutineSubcommand;
                let subcommand = match &command.command {
                    RoutineSubcommand::List(_) => "list",
                    RoutineSubcommand::Show(_) => "show",
                    RoutineSubcommand::Pause(_) => "pause",
                    RoutineSubcommand::Resume(_) => "resume",
                    RoutineSubcommand::Init(_) => "init",
                };
                CommandOperation::new(
                    RuntimeNeed::Forbidden,
                    Some(admin_meta(
                        "routine",
                        Some(subcommand),
                        Some("routine"),
                        None,
                    )),
                    None,
                    false,
                    dispatch_routine,
                )
            }
            Commands::Task(command) => {
                use super::locks::LocksSubcommand;
                use super::task::TaskPublicationSubcommand;
                use super::task::TaskSubcommand;
                use super::task::artifact::TaskArtifactSubcommand;
                let (subcommand, target_type, target_id) = match &command.command {
                    TaskSubcommand::Add(_) => ("add", Some("task"), None),
                    TaskSubcommand::Artifact(command) => match &command.command {
                        TaskArtifactSubcommand::Put(args) => {
                            ("artifact-put", Some("task"), Some(args.id.as_str()))
                        }
                        TaskArtifactSubcommand::Get(args) => {
                            ("artifact-get", Some("task"), Some(args.id.as_str()))
                        }
                    },
                    TaskSubcommand::Locks(command) => match &command.command {
                        LocksSubcommand::List(_) => ("locks-list", None, None),
                        LocksSubcommand::Contention(_) => ("locks-contention", None, None),
                        LocksSubcommand::Reserve(_) => ("locks-reserve", None, None),
                        LocksSubcommand::Release(args) => (
                            "locks-release",
                            Some("reservation"),
                            Some(args.reservation_id.as_str()),
                        ),
                    },
                    TaskSubcommand::List(_) => ("list", None, None),
                    TaskSubcommand::Flow(_) => ("flow", None, None),
                    TaskSubcommand::Show(args) => ("show", Some("task"), Some(args.id.as_str())),
                    TaskSubcommand::Lint(args) => ("lint", Some("task"), args.id.as_deref()),
                    TaskSubcommand::Update(args) => {
                        ("update", Some("task"), Some(args.id.as_str()))
                    }
                    TaskSubcommand::Start(args) => ("start", Some("task"), Some(args.id.as_str())),
                    TaskSubcommand::Archive(args) => {
                        ("archive", Some("task"), Some(args.id.as_str()))
                    }
                    TaskSubcommand::Export(_) => ("export", None, None),
                    TaskSubcommand::Import(args) => (
                        "import",
                        None,
                        Some(args.archive.to_str().unwrap_or_default()),
                    ),
                    TaskSubcommand::Publication(command) => match &command.command {
                        TaskPublicationSubcommand::Publish(_) => {
                            ("publication-publish", Some("workspace"), None)
                        }
                        TaskPublicationSubcommand::Status(_) => {
                            ("publication-status", Some("workspace"), None)
                        }
                        TaskPublicationSubcommand::Inspect(_) => {
                            ("publication-inspect", Some("publication"), None)
                        }
                        TaskPublicationSubcommand::Restore(_) => {
                            ("publication-restore", Some("workspace"), None)
                        }
                    },
                    TaskSubcommand::Reindex(_) => ("reindex", None, None),
                };
                let runtime_need = match &command.command {
                    TaskSubcommand::Show(args) => RuntimeNeed::TaskOwner {
                        task_id: args.id.clone(),
                    },
                    // `orbit.task.artifact.get` shares `orbit.task.show`'s
                    // resolved-globally-by-default schema wording, so `orbit
                    // task artifact get` must resolve the same way rather than
                    // reporting a routing refusal as `task_not_found`
                    // [ORB-12263].
                    TaskSubcommand::Artifact(command) => match &command.command {
                        TaskArtifactSubcommand::Get(args) => RuntimeNeed::TaskOwner {
                            task_id: args.id.clone(),
                        },
                        TaskArtifactSubcommand::Put(_) => RuntimeNeed::Required,
                    },
                    // Every other task verb keeps cwd (or `--workspace`) as its
                    // binding: only a read addressed by a globally unique ID can
                    // be routed from the ID alone.
                    _ => RuntimeNeed::Required,
                };
                CommandOperation::new(
                    runtime_need,
                    Some(admin_meta("task", Some(subcommand), target_type, target_id)),
                    None,
                    false,
                    boxed_runtime_dispatch!(Task),
                )
                .governed_when(
                    matches!(
                        &command.command,
                        TaskSubcommand::Publication(publication)
                            if matches!(
                                publication.command,
                                TaskPublicationSubcommand::Publish(_)
                                    | TaskPublicationSubcommand::Restore(_)
                            )
                    ),
                    "task",
                    subcommand,
                )
            }
            Commands::Search(command) => CommandOperation::new(
                RuntimeNeed::Required,
                Some(admin_meta(
                    "search",
                    Some(&command.audit_subcommand()),
                    Some("search"),
                    None,
                )),
                command.json.then_some(true),
                false,
                runtime_dispatch!(Search),
            ),
            Commands::Docs(command) => {
                use super::docs::DocsSubcommand;
                let (subcommand, target_id, json) = match &command.command {
                    DocsSubcommand::List(args) => ("list", None, args.json),
                    DocsSubcommand::Show(args) => ("show", Some(args.path.as_str()), args.json),
                    DocsSubcommand::Add(args) => ("add", Some(args.path.as_str()), args.json),
                    DocsSubcommand::Index(args) => ("index", None, args.json),
                    DocsSubcommand::Migrate(args) => ("migrate", None, args.json),
                };
                CommandOperation::new(
                    RuntimeNeed::Required,
                    Some(admin_meta(
                        "docs",
                        Some(subcommand),
                        Some("docs"),
                        target_id,
                    )),
                    json.then_some(true),
                    false,
                    runtime_dispatch!(Docs),
                )
            }
            // ADR-0209 bearing 1 [ORB-10358]: friction is registry-driven, so
            // this arm reads the invocation instead of matching verb by verb.
            // A new friction verb needs no edit here.
            Commands::Friction(command) => {
                let invocation = &command.command;
                CommandOperation::new(
                    RuntimeNeed::Required,
                    Some(admin_meta(
                        "friction",
                        Some(invocation.spec.name),
                        Some("friction"),
                        invocation.target_id(),
                    )),
                    invocation.json.then_some(true),
                    false,
                    runtime_dispatch!(Friction),
                )
            }
            // [ORB-11332] Operation mode is registry-driven like friction; the
            // governed verbs are enforced at the tool chokepoint, not here.
            Commands::Operation(command) => {
                let invocation = &command.command;
                CommandOperation::new(
                    RuntimeNeed::Required,
                    Some(admin_meta(
                        "operation",
                        Some(invocation.spec.name),
                        Some("operation"),
                        invocation.target_id(),
                    )),
                    invocation.json.then_some(true),
                    false,
                    runtime_dispatch!(Operation),
                )
            }
            Commands::Audit(command) => {
                use super::audit::AuditSubcommand;
                // `audit` emits no command-level audit row (it would audit
                // reads of the audit log), so a denial here is recorded only by
                // the authorization row the decision itself writes.
                let prunes =
                    matches!(&command.command, AuditSubcommand::Prune(args) if args.confirm);
                CommandOperation::new(
                    RuntimeNeed::Required,
                    None,
                    None,
                    false,
                    runtime_dispatch!(Audit),
                )
                .governed_when(prunes, "audit", "prune")
            }
            Commands::Log(_) => CommandOperation::new(
                RuntimeNeed::Required,
                Some(admin_meta("log", Some("tail"), Some("log_feed"), None)),
                None,
                false,
                runtime_dispatch!(Log),
            ),
            Commands::Doctor(_) => CommandOperation::new(
                RuntimeNeed::Required,
                Some(admin_meta("doctor", None, Some("workspace"), None)),
                None,
                false,
                runtime_dispatch!(Doctor),
            ),
            Commands::AutoTask(command) => {
                use super::auto_task::AutoTaskSubcommand;
                let (subcommand, target_id) = match &command.command {
                    AutoTaskSubcommand::Add(args) => ("add", Some(args.name.as_str())),
                    AutoTaskSubcommand::List(_) => ("list", None),
                    AutoTaskSubcommand::Show(args) => ("show", Some(args.name.as_str())),
                    AutoTaskSubcommand::Update(args) => ("update", Some(args.name.as_str())),
                    AutoTaskSubcommand::Toggle(args) => ("toggle", Some(args.name.as_str())),
                    AutoTaskSubcommand::Mint(args) => ("mint", Some(args.name.as_str())),
                };
                CommandOperation::new(
                    RuntimeNeed::Required,
                    Some(admin_meta(
                        "auto-task",
                        Some(subcommand),
                        Some("auto_task"),
                        target_id,
                    )),
                    None,
                    false,
                    runtime_dispatch!(AutoTask),
                )
            }
            Commands::Activity(command) => {
                use super::activity::ActivitySubcommand;
                let subcommand = match &command.command {
                    ActivitySubcommand::List(_) => "list",
                };
                CommandOperation::new(
                    RuntimeNeed::Required,
                    Some(admin_meta(
                        "activity",
                        Some(subcommand),
                        Some("activity"),
                        None,
                    )),
                    None,
                    false,
                    runtime_dispatch!(Activity),
                )
            }
            Commands::Job(command) => {
                use super::job::JobSubcommand;
                let (subcommand, target_id, job_run_id) = match &command.command {
                    JobSubcommand::List(_) => ("list", None, None),
                    JobSubcommand::Show(args) => ("show", Some(args.job_id.as_str()), None),
                    JobSubcommand::Run(args) => ("run", Some(args.job_id.as_str()), None),
                    JobSubcommand::Replay(args) => (
                        "replay",
                        Some(args.run_id.as_str()),
                        Some(args.run_id.as_str()),
                    ),
                    JobSubcommand::Resume(args) => (
                        "resume",
                        Some(args.run_id.as_str()),
                        Some(args.run_id.as_str()),
                    ),
                    JobSubcommand::RunPipelineWorker(args) => (
                        "run-pipeline-worker",
                        Some(args.run_id.as_str()),
                        Some(args.run_id.as_str()),
                    ),
                };
                let target_type =
                    if matches!(subcommand, "replay" | "resume" | "run-pipeline-worker") {
                        "job_run"
                    } else {
                        "job"
                    };
                let mut meta = admin_meta("job", Some(subcommand), Some(target_type), target_id);
                meta.job_run_id = job_run_id.map(String::from);
                CommandOperation::new(
                    RuntimeNeed::Required,
                    Some(meta),
                    None,
                    false,
                    runtime_dispatch!(Job),
                )
            }
            Commands::Tool(command) => {
                use super::tool::ToolSubcommand;
                let (subcommand, tool_name, target_type, target_id, role, json_output) =
                    match &command.command {
                        ToolSubcommand::Run(args) => (
                            "run",
                            Some(args.name.clone()),
                            Some("tool".to_string()),
                            Some(args.name.clone()),
                            tool_run_actor_role(args),
                            Some(args.pretty),
                        ),
                        ToolSubcommand::List(_) => {
                            ("list", None, None, None, "admin".to_string(), None)
                        }
                        ToolSubcommand::Show(args) => (
                            "show",
                            Some(args.name.clone()),
                            Some("tool".to_string()),
                            Some(args.name.clone()),
                            "admin".to_string(),
                            None,
                        ),
                        ToolSubcommand::Add(args) => (
                            "add",
                            args.name.clone(),
                            Some("tool".to_string()),
                            args.name.clone(),
                            "admin".to_string(),
                            None,
                        ),
                        ToolSubcommand::Scaffold(args) => (
                            "scaffold",
                            args.name.clone(),
                            Some("tool".to_string()),
                            args.name.clone().or_else(|| Some(args.path.clone())),
                            "admin".to_string(),
                            None,
                        ),
                        ToolSubcommand::Remove(args) => (
                            "remove",
                            Some(args.name.clone()),
                            Some("tool".to_string()),
                            Some(args.name.clone()),
                            "admin".to_string(),
                            None,
                        ),
                        ToolSubcommand::Enable(args) => (
                            "enable",
                            Some(args.name.clone()),
                            Some("tool".to_string()),
                            Some(args.name.clone()),
                            "admin".to_string(),
                            None,
                        ),
                        ToolSubcommand::Disable(args) => (
                            "disable",
                            Some(args.name.clone()),
                            Some("tool".to_string()),
                            Some(args.name.clone()),
                            "admin".to_string(),
                            None,
                        ),
                        ToolSubcommand::Doctor => {
                            ("doctor", None, None, None, "admin".to_string(), None)
                        }
                    };
                let runtime_need = match &command.command {
                    ToolSubcommand::Run(args) => match args.id_resolved_task_id() {
                        Some(task_id) => RuntimeNeed::TaskOwner { task_id },
                        None => RuntimeNeed::Required,
                    },
                    _ => RuntimeNeed::Required,
                };
                CommandOperation::new(
                    runtime_need,
                    Some(CommandMeta {
                        command: "tool".to_string(),
                        subcommand: Some(subcommand.to_string()),
                        tool_name,
                        target_type,
                        target_id,
                        role,
                        arguments_json: None,
                        job_run_id: None,
                    }),
                    json_output,
                    false,
                    runtime_dispatch!(Tool),
                )
            }
            Commands::Policy(command) => {
                use super::policy::PolicySubcommand;
                let (subcommand, target_id) = match &command.command {
                    PolicySubcommand::List(_) => ("list", None),
                    PolicySubcommand::Show(args) => ("show", Some(args.name.as_str())),
                    PolicySubcommand::Check(args) => ("check", Some(args.profile_name.as_str())),
                };
                CommandOperation::new(
                    RuntimeNeed::Required,
                    Some(admin_meta(
                        "policy",
                        Some(subcommand),
                        Some("policy"),
                        target_id,
                    )),
                    None,
                    false,
                    runtime_dispatch!(Policy),
                )
            }
            Commands::Executor(command) => {
                use super::executor::ExecutorSubcommand;
                let (subcommand, target_id) = match &command.command {
                    ExecutorSubcommand::List(_) => ("list", None),
                    ExecutorSubcommand::Show(args) => ("show", Some(args.name.as_str())),
                };
                CommandOperation::new(
                    RuntimeNeed::Required,
                    Some(admin_meta(
                        "executor",
                        Some(subcommand),
                        Some("executor"),
                        target_id,
                    )),
                    None,
                    false,
                    runtime_dispatch!(Executor),
                )
            }
            Commands::Mcp(command) => {
                use super::mcp::McpSubcommand;
                let subcommand = match &command.command {
                    McpSubcommand::Init(_) => "init",
                    McpSubcommand::Remove(_) => "remove",
                    McpSubcommand::Serve(_) => "serve",
                    McpSubcommand::Listen(_) => "listen",
                    McpSubcommand::Callers(_) => "callers",
                };
                CommandOperation::new(
                    RuntimeNeed::Forbidden,
                    Some(admin_meta("mcp", Some(subcommand), Some("mcp"), None)),
                    None,
                    false,
                    dispatch_mcp,
                )
            }
            Commands::Web(command) => {
                use super::web::WebSubcommand;
                let subcommand = match &command.command {
                    WebSubcommand::Serve(_) => "serve",
                    WebSubcommand::Connect(_) => "connect",
                };
                CommandOperation::new(
                    RuntimeNeed::Forbidden,
                    Some(admin_meta("web", Some(subcommand), Some("dashboard"), None)),
                    None,
                    false,
                    dispatch_web,
                )
            }
            Commands::Skill(command) => {
                use super::skill::SkillSubcommand;
                let (subcommand, target_id) = match &command.command {
                    SkillSubcommand::List(_) => ("list", None),
                    SkillSubcommand::Show(args) => ("show", Some(args.name.as_str())),
                    SkillSubcommand::Doctor(_) => ("doctor", None),
                    SkillSubcommand::Link(_) => ("link", None),
                    SkillSubcommand::Unlink(_) => ("unlink", None),
                };
                CommandOperation::new(
                    RuntimeNeed::Required,
                    Some(admin_meta(
                        "skill",
                        Some(subcommand),
                        Some("skill"),
                        target_id,
                    )),
                    None,
                    false,
                    runtime_dispatch!(Skill),
                )
            }
            Commands::Logs(command) => CommandOperation::new(
                RuntimeNeed::Required,
                Some(admin_meta(
                    "logs",
                    None,
                    Some("job_run"),
                    Some(&command.run_id),
                )),
                None,
                false,
                runtime_dispatch!(Logs),
            ),
            Commands::Artifacts(command) => CommandOperation::new(
                RuntimeNeed::Required,
                Some(admin_meta(
                    "artifacts",
                    None,
                    Some(if command.task { "task" } else { "job_run" }),
                    Some(&command.id),
                )),
                None,
                false,
                runtime_dispatch!(Artifacts),
            ),
        }
    }
}

fn dispatch_init(command: Commands, context: DispatchContext<'_>) -> CommandOut {
    match command {
        Commands::Init(command) => command.execute_without_runtime(context.root_override),
        _ => dispatch_mismatch("Init"),
    }
}

fn dispatch_host(command: Commands, context: DispatchContext<'_>) -> CommandOut {
    use super::host::{HostCommand, HostSubcommand};
    match command {
        Commands::Host(HostCommand {
            command: HostSubcommand::Show(args),
        }) => args.execute_without_runtime(context.root_override),
        Commands::Host(command) => command.execute(context.runtime()?),
        _ => dispatch_mismatch("Host"),
    }
}

fn dispatch_workspace(command: Commands, context: DispatchContext<'_>) -> CommandOut {
    use super::workspace::{WorkspaceCommand, WorkspaceSubcommand};
    match command {
        Commands::Workspace(WorkspaceCommand {
            command: WorkspaceSubcommand::Init(args),
        }) => args.execute_without_runtime(context.root_override),
        Commands::Workspace(WorkspaceCommand {
            command: WorkspaceSubcommand::Sync(args),
        }) => args.execute_without_runtime(context.root_override),
        Commands::Workspace(command) => command.execute(context.runtime()?),
        _ => dispatch_mismatch("Workspace"),
    }
}

fn dispatch_mcp(command: Commands, context: DispatchContext<'_>) -> CommandOut {
    use super::mcp::{McpCommand, McpSubcommand};
    match command {
        Commands::Mcp(McpCommand {
            command: McpSubcommand::Init(args),
        }) => args.execute_without_runtime(context.root_override),
        Commands::Mcp(McpCommand {
            command: McpSubcommand::Remove(args),
        }) => args.execute_without_runtime(context.root_override),
        Commands::Mcp(McpCommand {
            command: McpSubcommand::Serve(args),
        }) => args.execute_without_runtime(context.root_override),
        Commands::Mcp(McpCommand {
            command: McpSubcommand::Listen(args),
        }) => args.execute_without_runtime(context.root_override),
        Commands::Mcp(McpCommand {
            command: McpSubcommand::Callers(args),
        }) => args.execute_without_runtime(context.root_override),
        _ => dispatch_mismatch("Mcp"),
    }
}

fn dispatch_migrate(command: Commands, context: DispatchContext<'_>) -> CommandOut {
    match command {
        Commands::Migrate(command) if !command.confirm => {
            command.execute_without_runtime(context.root_override, context.workspace_selector)
        }
        Commands::Migrate(command) => command.execute(context.runtime()?),
        _ => dispatch_mismatch("Migrate"),
    }
}

fn dispatch_update(command: Commands, context: DispatchContext<'_>) -> CommandOut {
    match command {
        Commands::Update(command) => command.execute_without_runtime(context.root_override),
        _ => dispatch_mismatch("Update"),
    }
}

fn dispatch_run(command: Commands, context: DispatchContext<'_>) -> CommandOut {
    use super::run::{RunCommand, RunSubcommand};
    match command {
        Commands::Run(RunCommand {
            command: RunSubcommand::ShipSweep(args),
        }) => args.execute_without_runtime(),
        Commands::Run(command) => command.execute(context.runtime()?),
        _ => dispatch_mismatch("Run"),
    }
}

fn dispatch_sweep(command: Commands, context: DispatchContext<'_>) -> CommandOut {
    match command {
        Commands::Sweep(command) => {
            command.execute_without_runtime(context.root_override, context.workspace_selector)
        }
        _ => dispatch_mismatch("Sweep"),
    }
}

fn dispatch_clock(command: Commands, context: DispatchContext<'_>) -> CommandOut {
    match command {
        Commands::Clock(command) => {
            command.execute_without_runtime(context.root_override, context.workspace_selector)
        }
        _ => dispatch_mismatch("Clock"),
    }
}

fn dispatch_routine(command: Commands, context: DispatchContext<'_>) -> CommandOut {
    match command {
        Commands::Routine(command) => command.execute_without_runtime(context.root_override),
        _ => dispatch_mismatch("Routine"),
    }
}

fn dispatch_web(command: Commands, context: DispatchContext<'_>) -> CommandOut {
    use super::web::{WebCommand, WebSubcommand};
    match command {
        Commands::Web(WebCommand {
            command: WebSubcommand::Serve(args),
        }) => {
            orbit_web::serve_from_env(args, context.root_override)?;
            Ok(CommandOutput::Silent)
        }
        Commands::Web(WebCommand {
            command: WebSubcommand::Connect(args),
        }) => {
            orbit_web::connect(args, context.root_override)?;
            Ok(CommandOutput::Silent)
        }
        _ => dispatch_mismatch("Web"),
    }
}

fn tool_run_actor_role(args: &super::tool::ToolRunArgs) -> String {
    let (input_agent, input_model) = tool_run_input_identity(args);
    let env_agent = std::env::var("ORBIT_AGENT_NAME")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let env_model = std::env::var("ORBIT_AGENT_MODEL")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let has_input_identity = input_agent.is_some() || input_model.is_some();
    let has_flag_identity = args.agent.is_some() || args.model.is_some();
    let (agent, model) = if has_input_identity {
        (input_agent, input_model)
    } else if has_flag_identity {
        (args.agent.clone(), args.model.clone())
    } else {
        (env_agent, env_model)
    };
    let agent = normalize_agent_family_for_model(agent.as_deref(), model.as_deref())
        .ok()
        .flatten()
        .or(agent);

    normalize_optional_attribution_label(model.as_deref().or(agent.as_deref()), model.as_deref())
        .unwrap_or_else(|| "agent".to_string())
}

fn tool_run_input_identity(args: &super::tool::ToolRunArgs) -> (Option<String>, Option<String>) {
    let value = args
        .input
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .or_else(|| {
            args.input_file.as_deref().and_then(|path| {
                std::fs::read_to_string(path)
                    .ok()
                    .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            })
        });

    match value {
        Some(Value::Object(map)) => (
            map.get("agent")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
            map.get("model")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
        ),
        _ => (None, None),
    }
}
