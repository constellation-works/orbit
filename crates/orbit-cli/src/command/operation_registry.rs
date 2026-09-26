use super::*;

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
                use super::super::workspace::WorkspacePublicationSubcommand;
                use super::super::workspace::WorkspaceSourceRemoteSubcommand;
                use super::super::workspace::WorkspaceSubcommand;
                let (subcommand, runtime_need, governed) = match &command.command {
                    WorkspaceSubcommand::Init(_) => ("init", RuntimeNeed::Forbidden, false),
                    WorkspaceSubcommand::Sync(_) => ("sync", RuntimeNeed::Forbidden, false),
                    WorkspaceSubcommand::List(_) => ("list", RuntimeNeed::ReadOnly, false),
                    WorkspaceSubcommand::Show(_) => ("show", RuntimeNeed::ReadOnly, false),
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
            Commands::Config(command) => {
                use super::super::config::ConfigSubcommand;
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
                use super::super::run::RunSubcommand;
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
                    RunSubcommand::TaskPilot(_) => (
                        "task-pilot",
                        Some("workflow"),
                        Some("task-pilot"),
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
                        RuntimeNeed::ReadOnly,
                    ),
                    RunSubcommand::Show(args) => (
                        "show",
                        Some("job_run"),
                        args.run_id.as_deref(),
                        RuntimeNeed::ReadOnly,
                    ),
                    RunSubcommand::Logs(args) => (
                        "logs",
                        Some("job_run"),
                        args.run_id.as_deref(),
                        observation_runtime_need(args.no_reconcile),
                    ),
                    RunSubcommand::Events(args) => (
                        "events",
                        Some("job_run"),
                        args.run_id.as_deref(),
                        observation_runtime_need(args.no_reconcile),
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
                use super::super::gc::GcTarget;
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
                use super::super::clock::ClockSubcommand;
                let subcommand = match &command.command {
                    ClockSubcommand::Status => "status",
                    ClockSubcommand::Pause => "pause",
                    ClockSubcommand::Enable => "enable",
                    ClockSubcommand::Repair => "repair",
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
                use super::super::routine::RoutineSubcommand;
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
                use super::super::locks::LocksSubcommand;
                use super::super::task::TaskPublicationSubcommand;
                use super::super::task::TaskSubcommand;
                use super::super::task::artifact::TaskArtifactSubcommand;
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
                let task_owner_id = match &command.command {
                    TaskSubcommand::Show(args) => Some(args.id.clone()),
                    _ => None,
                };
                let runtime_need = match &command.command {
                    TaskSubcommand::Show(_) => RuntimeNeed::ReadOnly,
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
                    TaskSubcommand::List(_) | TaskSubcommand::Flow(_) => RuntimeNeed::ReadOnly,
                    TaskSubcommand::Lint(args) if !args.restore_pruned => RuntimeNeed::ReadOnly,
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
                .with_task_owner_id(task_owner_id)
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
                if command.command.is_some() {
                    RuntimeNeed::Required
                } else {
                    RuntimeNeed::ReadOnly
                },
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
            // ADR-0209 bearing 1 [ORB-10358]: friction is registry-driven, so
            // this arm reads the invocation instead of matching verb by verb.
            // A new friction verb needs no edit here.
            Commands::Friction(command) => {
                use orbit_common::governance::friction::FrictionVerb;
                let invocation = &command.command;
                let runtime_need = if invocation.spec.verb == FrictionVerb::List {
                    RuntimeNeed::ReadOnly
                } else {
                    RuntimeNeed::Required
                };
                CommandOperation::new(
                    runtime_need,
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
            Commands::Audit(command) => {
                use super::super::audit::AuditSubcommand;
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
            Commands::Doctor(command) => {
                use super::super::doctor::DoctorSubcommand;
                // The focused diagnostics only read definitions, so they stay
                // available while another executable generation holds the
                // store; the health checks and repairs need the full runtime.
                let (subcommand, target_type, target_id, runtime_need) = match &command.command {
                    None => (None, "workspace", None, RuntimeNeed::Required),
                    Some(DoctorSubcommand::Providers(_)) => {
                        (Some("providers"), "executor", None, RuntimeNeed::ReadOnly)
                    }
                    Some(DoctorSubcommand::FsAccess(args)) => (
                        Some("fs-access"),
                        "policy",
                        Some(args.profile.as_str()),
                        RuntimeNeed::ReadOnly,
                    ),
                };
                CommandOperation::new(
                    runtime_need,
                    Some(admin_meta(
                        "doctor",
                        subcommand,
                        Some(target_type),
                        target_id,
                    )),
                    None,
                    false,
                    runtime_dispatch!(Doctor),
                )
            }
            Commands::AutoTask(command) => {
                use super::super::auto_task::AutoTaskSubcommand;
                let runtime_need = if matches!(
                    &command.command,
                    AutoTaskSubcommand::List(_) | AutoTaskSubcommand::Show(_)
                ) {
                    RuntimeNeed::ReadOnly
                } else {
                    RuntimeNeed::Required
                };
                let (subcommand, target_id) = match &command.command {
                    AutoTaskSubcommand::Add(args) => ("add", Some(args.name.as_str())),
                    AutoTaskSubcommand::List(_) => ("list", None),
                    AutoTaskSubcommand::Show(args) => ("show", Some(args.name.as_str())),
                    AutoTaskSubcommand::Update(args) => ("update", Some(args.name.as_str())),
                    AutoTaskSubcommand::Toggle(args) => ("toggle", Some(args.name.as_str())),
                    AutoTaskSubcommand::Mint(args) => ("mint", Some(args.name.as_str())),
                    AutoTaskSubcommand::Recover(args) => ("recover", Some(args.name.as_str())),
                    AutoTaskSubcommand::Reset(args) => ("reset", Some(args.name.as_str())),
                    AutoTaskSubcommand::Delete(args) => ("delete", Some(args.name.as_str())),
                    AutoTaskSubcommand::Restore(args) => ("restore", Some(args.name.as_str())),
                };
                CommandOperation::new(
                    runtime_need,
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
            Commands::Job(command) => {
                use super::super::job::JobSubcommand;
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
                let runtime_need = if matches!(command.command, JobSubcommand::RunPipelineWorker(_))
                {
                    RuntimeNeed::PipelineWorker
                } else {
                    RuntimeNeed::Required
                };
                CommandOperation::new(
                    runtime_need,
                    Some(meta),
                    None,
                    false,
                    runtime_dispatch!(Job),
                )
            }
            Commands::Tool(command) => tool_operation(command),
            // `orbit <ns> <verb>` is `orbit tool run <ns>.<verb>` in another
            // spelling (§4.6), so it declares the same operation: the same
            // runtime need, the same audit row, and the same dispatch into
            // `ToolRunArgs`. A second, group-specific policy here is exactly
            // the drift the two spellings must not have.
            Commands::PluginGroup(invocation) => {
                let args = &invocation.tool_run;
                let runtime_need = match args.id_resolved_task_id() {
                    Some(task_id) => RuntimeNeed::TaskOwner { task_id },
                    None => RuntimeNeed::Required,
                };
                CommandOperation::new(
                    runtime_need,
                    Some(CommandMeta {
                        command: "tool".to_string(),
                        subcommand: Some("run".to_string()),
                        tool_name: Some(args.name.clone()),
                        target_type: Some("tool".to_string()),
                        target_id: Some(args.name.clone()),
                        role: tool_run_actor_role(args),
                        arguments_json: None,
                        job_run_id: None,
                    }),
                    Some(args.pretty),
                    false,
                    |command, context| match command {
                        Commands::PluginGroup(invocation) => {
                            (*invocation).execute(context.runtime()?)
                        }
                        _ => dispatch_mismatch("PluginGroup"),
                    },
                )
                .plugin_callback_entry_point(true)
            }
            Commands::Plugin(command) => {
                use super::super::plugin::PluginSubcommand;
                let (subcommand, target_id) = match &command.command {
                    PluginSubcommand::Add(args) => ("add", Some(args.source.clone())),
                    PluginSubcommand::Upgrade(args) => ("upgrade", Some(args.name.clone())),
                    PluginSubcommand::Enable(args) => ("enable", Some(args.name.clone())),
                    PluginSubcommand::Disable(args) => ("disable", Some(args.name.clone())),
                    PluginSubcommand::Remove(args) => ("remove", Some(args.name.clone())),
                    PluginSubcommand::List(_) => ("list", None),
                    PluginSubcommand::Show(args) => ("show", Some(args.name.clone())),
                    PluginSubcommand::Doctor => ("doctor", None),
                    PluginSubcommand::Validate(args) => {
                        ("validate", Some(args.dir.to_string_lossy().into_owned()))
                    }
                    PluginSubcommand::Test(args) => {
                        ("test", Some(args.dir.to_string_lossy().into_owned()))
                    }
                    PluginSubcommand::Scaffold(args) => ("scaffold", Some(args.namespace.clone())),
                    PluginSubcommand::Sync(_) => ("sync", None),
                    PluginSubcommand::Migrate(args) => ("migrate", Some(args.binary.clone())),
                };
                // `list`, `show`, `doctor`, `validate` and `scaffold` only
                // read Orbit state — `scaffold` writes a new directory, never
                // a record — while `test` records the certification it earned
                // and the rest write the host's plugin records or install root.
                let runtime_need = match &command.command {
                    PluginSubcommand::List(_)
                    | PluginSubcommand::Show(_)
                    | PluginSubcommand::Doctor
                    | PluginSubcommand::Scaffold(_)
                    | PluginSubcommand::Validate(_) => RuntimeNeed::ReadOnly,
                    PluginSubcommand::Sync(args) if args.dry_run => RuntimeNeed::ReadOnly,
                    _ => RuntimeNeed::Required,
                };
                CommandOperation::new(
                    runtime_need,
                    Some(admin_meta(
                        "plugin",
                        Some(subcommand),
                        Some("plugin"),
                        target_id.as_deref(),
                    )),
                    None,
                    false,
                    runtime_dispatch!(Plugin),
                )
            }
            Commands::Mcp(command) => {
                use super::super::mcp::McpSubcommand;
                let subcommand = match &command.command {
                    McpSubcommand::Init(_) => "init",
                    McpSubcommand::Remove(_) => "remove",
                    McpSubcommand::Serve(_) => "serve",
                    McpSubcommand::Listen(_) => "listen",
                };
                CommandOperation::new(
                    RuntimeNeed::Forbidden,
                    Some(admin_meta("mcp", Some(subcommand), Some("mcp"), None)),
                    None,
                    false,
                    dispatch_mcp,
                )
                // Only `serve`: a stdio server a backend starts for itself
                // answers `tools/call` through the same callback allowlist.
                // `init`/`remove` rewrite client configs and `listen` opens a
                // TCP port, neither of which is a callback.
                .plugin_callback_entry_point(matches!(&command.command, McpSubcommand::Serve(_)))
            }
            Commands::Web(command) => {
                use super::super::web::WebSubcommand;
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
                use super::super::skill::SkillSubcommand;
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
