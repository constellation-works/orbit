use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
use orbit_core::ActorIdentity;
use orbit_core::composition::pin_executable_generation_as;

use super::classify::{
    claimed_owner_tool, command_rotates_jsonl_on_start, handover_capability, is_clock_tick,
    participant_role, report_refused_clock_tick,
};
use super::discover::{explicit_root_source, parse_cli};
use super::finish::{finish_command, print_error, refuse_plugin_child_cli};
use crate::audit_middleware;
use crate::command;
use crate::command::operation::{CommandOperation, DispatchContext, RuntimeNeed};
use crate::output;
use crate::output::sink::{OutputMode, OutputSink};
use crate::root_check;

pub(crate) fn run() {
    // This is the production entry point for CLI, MCP, sweep clock, and the
    // dashboard (`orbit web serve`). Test harnesses never execute this main.
    orbit_core::mark_process_as_pipeline_worker_binary();
    // Provider lines are retained for `run logs --follow`, while diagnostics
    // stay quiet on stderr. An explicit RUST_LOG still overrides both defaults.
    orbit_common::observability::logging::init_subscriber_with_file_filter(
        "warn",
        "warn,orbit_engine::activity_job::cli_runner::supervisor=info,orbit::mcp_config=info",
    );
    output::pipe::install_handler();

    let (mut cli, requested_format, legacy_json, header) = parse_cli();
    if command_rotates_jsonl_on_start(&cli.command) {
        orbit_common::observability::logging::rotate_global_jsonl_best_effort();
    }
    // Resolved once per invocation, before dispatch, and passed to the one
    // renderer that consumes it. Nothing downstream re-derives these answers.
    let sink = OutputSink::from_process(requested_format, legacy_json).with_plain_header(header);
    sink.apply_color_policy();
    tracing::debug!(
        mode = ?sink.mode(),
        is_tty = sink.is_tty(),
        width = sink.width(),
        color_allowed = sink.color_allowed(),
        progress_allowed = sink.progress_allowed(),
        "resolved output sink"
    );
    // Update owns exclusive admission and pins its candidate before convergence.
    // Read-only commands may join a live generation without rewriting the
    // record when store schema matches. Everything else pins the exact running
    // inode before any bootstrap. This also covers all MCP transports, managed
    // workers and automatic migrations. `--root` / `ORBIT_ROOT` isolate that
    // pin so a read-only unpinned `~/.orbit` cannot block scratch init. A
    // managed macOS child joins its parent's host registry pin even when
    // ORBIT_ROOT selects workspace data; update still checks both roots.
    let actor = ActorIdentity::from_env();
    // ORB-12876: a recognized plugin backend reaches Orbit only through a tool
    // call, which the callback allowlist gates against the plugin's
    // `permissions.orbit_tools`. Refuse it the rest of the CLI here — before
    // host routing, generation pinning, runtime bootstrap and dispatch — so no
    // plain command reads governed data around that allowlist.
    {
        let operation = cli.command.operation().attribute_to(&actor);
        if std::env::var_os(
            orbit_common::security::application_credential::APPLICATION_CREDENTIAL_FD_ENV,
        )
        .is_some()
            && operation.audit_meta.as_ref().is_none_or(|meta| {
                meta.command != "tool" || meta.subcommand.as_deref() != Some("run")
            })
        {
            print_error(
                &orbit_core::OrbitError::CapabilityDenied(
                    "application credentials may invoke only workspace tool calls".into(),
                ),
                &sink,
                operation.json_error_preference,
            );
            orbit_common::observability::logging::exit(1);
        }
        if !operation.plugin_callback_entry_point
            && let Err(error) = refuse_plugin_child_cli(&operation.audit_meta, cli.root.as_deref())
        {
            print_error(&error, &sink, operation.json_error_preference);
            orbit_common::observability::logging::exit(1);
        }
    }
    // A task id another host's prefix names, a selector another host lists,
    // or `--host` naming one: deliver there and open nothing here. `--host`
    // naming this machine rewrites the selector and runs on [ORB-14449].
    match command::host_route::preflight(&mut cli) {
        Ok(command::host_route::Preflight::Local) => {}
        Ok(command::host_route::Preflight::Remote(result)) => {
            let json_error_preference = cli.command.operation().json_error_preference;
            finish_command(result, &sink, json_error_preference, None);
            return;
        }
        Err(error) => {
            print_error(&error, &sink, cli.command.operation().json_error_preference);
            orbit_common::observability::logging::exit(1);
        }
    }
    let inspection =
        matches!(&cli.command, command::Commands::Migrate(command) if !command.confirm);
    let root_override = cli.root.clone();
    let workspace_selector = cli.workspace.clone().or_else(|| {
        if let command::Commands::Task(task) = &cli.command {
            command::task::operation::retry_owner_workspace(&task.command).map(ToOwned::to_owned)
        } else {
            None
        }
    });
    let CommandOperation {
        runtime_need,
        task_owner_id,
        audit_meta,
        json_error_preference,
        dispatch,
        governed,
        plugin_callback_entry_point: _,
    } = cli.command.operation().attribute_to(&actor);
    let clock_tick = is_clock_tick(&cli.command);
    let quiet_clock_tick =
        clock_tick && !matches!(sink.mode(), OutputMode::Json | OutputMode::Ndjson);
    // Invalid `orbit tool run` input cannot run. Reporting it before generation
    // pin keeps a typo from resolving a workspace or applying pending migrations.
    let unusable_tool_input = matches!(runtime_need, RuntimeNeed::UnusableToolInput { .. });
    // `orbit lease token` and `orbit lease listen` run as the lease listener
    // user and touch only its own Orbit home, so they pin no generation there.
    let listener_user_command =
        matches!(&cli.command, command::Commands::Lease(lease) if lease.runs_as_listener_user());
    let _generation = if matches!(&cli.command, command::Commands::Update(_))
        || inspection
        || unusable_tool_input
        || listener_user_command
    {
        None
    } else {
        let root =
            match orbit_core::runtime::resolve_process_generation_root(root_override.as_deref()) {
                Ok(root) => root,
                Err(error) => {
                    print_error(&error, &sink, None);
                    orbit_common::observability::logging::exit(1);
                }
            };
        let root_source = explicit_root_source(root_override.as_deref());
        let root_existed = root.exists();
        if let Some(source) = root_source
            && let Err(error) = root_check::validate_explicit_root(&root, source)
        {
            print_error(&error, &sink, None);
            orbit_common::observability::logging::exit(1);
        }
        match pin_executable_generation_as(
            &root,
            matches!(
                runtime_need,
                RuntimeNeed::ReadOnly | RuntimeNeed::PluginReadOnly | RuntimeNeed::HostReadOnly
            ),
            participant_role(&cli.command),
            handover_capability(&cli.command),
        ) {
            Ok(guard) => {
                if clock_tick
                    && let Ok(digest) = orbit_common::fs::generation::process_generation()
                    && let Ok(Some(summary)) =
                        orbit_common::fs::generation::finish_clock_generation_hold(
                            &root,
                            digest,
                            chrono::Utc::now(),
                        )
                {
                    eprintln!("{summary}");
                }
                Some(guard)
            }
            Err(error) => {
                if clock_tick && orbit_common::fs::generation::is_clock_upgrade_refusal(&error) {
                    report_refused_clock_tick(&root, &error);
                } else if clock_tick
                    && orbit_common::fs::generation::is_clock_generation_hold(&error)
                    && let Ok(digest) = orbit_common::fs::generation::process_generation()
                    && orbit_common::fs::generation::record_clock_generation_hold(
                        &root,
                        digest,
                        chrono::Utc::now(),
                    )
                    .is_ok()
                    && quiet_clock_tick
                {
                    orbit_common::observability::logging::exit(1);
                }
                let error = match root_source {
                    Some(source) if !root_existed => {
                        root_check::missing_root_error(&root, source, &error)
                    }
                    _ => error,
                };
                print_error(&error, &sink, None);
                orbit_common::observability::logging::exit(1);
            }
        }
    };

    // A claimed worker addresses the owner's task by ID, but the follower's
    // task registry intentionally has no local copy. Bootstrap its owner
    // calls in the worker's bound checkout so Core can carry them over the
    // authenticated run broker. The worker binding is restored from the
    // host's protected invocation record; request fields never select this
    // path. ToolRunArgs validates any explicit selector against that binding.
    let claimed_owner_call = claimed_owner_tool(&cli.command);
    let claimed_owner_worker = if unusable_tool_input {
        false
    } else if claimed_owner_call {
        let global_root = match orbit_core::runtime::resolve_global_root() {
            Ok(root) => root,
            Err(error) => {
                print_error(&error, &sink, json_error_preference);
                orbit_common::observability::logging::exit(1);
            }
        };
        match orbit_core::OrbitRuntime::current_worker_invocation(&global_root) {
            Ok(binding) => binding
                .is_some_and(|binding| binding.execution.machine_id != binding.owner_machine_id),
            Err(error) => {
                print_error(&error, &sink, json_error_preference);
                orbit_common::observability::logging::exit(1);
            }
        }
    } else {
        false
    };
    if let command::Commands::Config(config) = &cli.command
        && let command::config::ConfigSubcommand::Set(args) = &config.command
        && let Err(error) = command::config::set::preflight_indirect_workspace_write(
            root_override.as_deref(),
            args.global,
            workspace_selector.is_some(),
        )
    {
        print_error(&error, &sink, json_error_preference);
        orbit_common::observability::logging::exit(1);
    }
    let bootstrapped = if let RuntimeNeed::UnusableToolInput { message } = &runtime_need {
        Err(orbit_core::OrbitError::InvalidInput(message.clone()))
    } else if claimed_owner_worker {
        RegisteredRuntimeFactory::initialize_with_overrides(root_override.as_deref(), None)
    } else {
        match &runtime_need {
            RuntimeNeed::UnusableToolInput { message } => {
                Err(orbit_core::OrbitError::InvalidInput(message.clone()))
            }
            RuntimeNeed::Forbidden => {
                // A runtime-forbidden command has no store to authorize or audit
                // against. None is governed; `Commands::operation` is exhaustive, so
                // a future one that is would have to resolve this first.
                debug_assert!(
                    governed.is_none(),
                    "a governed operation must be able to reach the authorization chokepoint"
                );
                let result = dispatch(
                    cli.command,
                    DispatchContext::without_runtime(
                        root_override.as_deref(),
                        workspace_selector.as_deref(),
                    ),
                );
                finish_command(result, &sink, json_error_preference, None);
                return;
            }
            RuntimeNeed::Required => RegisteredRuntimeFactory::initialize_with_overrides(
                root_override.as_deref(),
                workspace_selector.as_deref(),
            ),
            RuntimeNeed::SelectedWorkspace { selector } => {
                RegisteredRuntimeFactory::initialize_with_overrides(
                    root_override.as_deref(),
                    Some(selector),
                )
            }
            RuntimeNeed::PipelineWorker => {
                RegisteredRuntimeFactory::initialize_pipeline_worker_with_overrides(
                    root_override.as_deref(),
                    workspace_selector.as_deref(),
                )
            }
            RuntimeNeed::ReadOnly => match task_owner_id.as_deref() {
                Some(task_id) => orbit_cmd::task_owner::initialize_for_task_show(
                    root_override.as_deref(),
                    workspace_selector.as_deref(),
                    task_id,
                ),
                None => RegisteredRuntimeFactory::initialize_read_only_with_overrides(
                    root_override.as_deref(),
                    workspace_selector.as_deref(),
                ),
            },
            RuntimeNeed::PluginReadOnly => {
                RegisteredRuntimeFactory::initialize_plugin_read_only_with_overrides(
                    root_override.as_deref(),
                    workspace_selector.as_deref(),
                )
            }
            RuntimeNeed::HostReadOnly => {
                RegisteredRuntimeFactory::initialize_host_read_only_with_overrides(
                    root_override.as_deref(),
                    workspace_selector.as_deref(),
                )
            }
            RuntimeNeed::TaskOwner { task_id } => orbit_cmd::task_owner::initialize_for_task_show(
                root_override.as_deref(),
                workspace_selector.as_deref(),
                task_id,
            ),
        }
    };

    let runtime = match bootstrapped {
        Ok(runtime) => runtime,
        Err(err) => {
            print_error(&err, &sink, json_error_preference);
            orbit_common::observability::logging::exit(1);
        }
    }
    .with_actor(actor);

    let context = DispatchContext::with_runtime(
        &runtime,
        root_override.as_deref(),
        workspace_selector.as_deref(),
    );
    // ORB-10453: the CLI's single authorization chokepoint. Every command
    // traverses it before dispatch, so a governed operation cannot be reached
    // by adding a subcommand that forgets its own guard.
    let authorize = |runtime: &orbit_core::OrbitRuntime| match governed {
        Some(operation) => {
            runtime.authorize_command_operation(operation.command, operation.subcommand)
        }
        None => Ok(()),
    };
    // The runtime appends telemetry separately from write-free state handles.
    // Foreign-generation reads still need their native command outcome; the
    // guard itself suppresses emission when tool dispatch already owns it.
    let guard = audit_meta.map(|meta| audit_middleware::AuditGuard::new(&runtime, meta));
    let result = authorize(&runtime).and_then(|()| dispatch(cli.command, context));

    finish_command(result, &sink, json_error_preference, guard);
}
