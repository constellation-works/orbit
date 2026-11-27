use orbit_cmd::registry_runtime::{PreOpenRoute, RegisteredRuntimeFactory};
use orbit_core::app::InvocationAuditScope;
use orbit_core::app::host::pin_executable_generation_as;

use super::classify::{
    claimed_owner_tool, command_rotates_jsonl_on_start, handover_capability, is_clock_tick,
    participant_role, report_refused_clock_tick,
};
use super::discover::{explicit_root_source, parse_cli};
use super::finish::{finish_command, print_error, refuse_plugin_child_cli};
use super::resolved::Resolved;
use crate::command;
use crate::command::operation::{CommandOperation, DispatchContext, RuntimeNeed};
use crate::output;
use crate::output::common::sink::{OutputMode, OutputSink};
use crate::root_check;

pub(crate) fn run(env: orbit_common::Env) {
    // This is the production entry point for CLI, MCP, sweep clock, and the
    // dashboard (`orbit web serve`). Test harnesses never execute this main.
    orbit_core::app::host::install_captured_env(env.clone());
    orbit_core::app::run::mark_process_as_pipeline_worker_binary();
    // Provider lines are retained for `run logs --follow`, while diagnostics
    // stay quiet on stderr. An explicit RUST_LOG still overrides both defaults.
    orbit_common::observability::logging::init_subscriber_with_file_filter(
        &env,
        "warn",
        "warn,orbit_engine::activity_job::cli_runner::supervisor=info,orbit::mcp_config=info",
    );
    output::common::pipe::install_handler();

    let (mut cli, requested_format, legacy_json, header) = parse_cli(&env);
    if command_rotates_jsonl_on_start(&cli.command) {
        orbit_common::observability::logging::rotate_global_jsonl_best_effort(&env);
    }
    // Resolved once per invocation, before dispatch, and passed to the one
    // renderer that consumes it. Nothing downstream re-derives these answers.
    let sink =
        OutputSink::from_process(&env, requested_format, legacy_json).with_plain_header(header);
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
    let resolved = Resolved::resolve(&env, cli.root.as_deref());
    let actor = resolved.actor();
    // ORB-12876: a recognized plugin backend reaches Orbit only through a tool
    // call, which the callback allowlist gates against the plugin's
    // `permissions.orbit_tools`. Refuse it the rest of the CLI here — before
    // host routing, generation pinning, runtime bootstrap and dispatch — so no
    // plain command reads governed data around that allowlist.
    {
        let operation = cli.command.operation(&env).attribute_to(actor);
        if env.orbit_application_credential_fd().is_some()
            && operation.audit_meta.as_ref().is_none_or(|meta| {
                meta.command != "tool" || meta.subcommand.as_deref() != Some("run")
            })
        {
            print_error(
                &orbit_common::OrbitError::CapabilityDenied(
                    "application credentials may invoke only workspace tool calls".into(),
                ),
                &sink,
                operation.json_error_preference,
            );
            orbit_common::observability::logging::exit(1);
        }
        if !operation.plugin_callback_entry_point
            && let Err(error) = refuse_plugin_child_cli(&operation.audit_meta, &resolved, &env)
        {
            print_error(&error, &sink, operation.json_error_preference);
            orbit_common::observability::logging::exit(1);
        }
    }
    // A task id another host's prefix names, a selector another host lists,
    // or `--host` naming one: deliver there and open nothing here. `--host`
    // naming this machine rewrites the selector and runs on [ORB-14449].
    match command::host_route::preflight(&mut cli, &env, &resolved) {
        Ok(command::host_route::Preflight::Local) => {}
        Ok(command::host_route::Preflight::Remote(result)) => {
            let json_error_preference = cli.command.operation(&env).json_error_preference;
            finish_command(result, &sink, json_error_preference, None);
            return;
        }
        Err(error) => {
            print_error(
                &error,
                &sink,
                cli.command.operation(&env).json_error_preference,
            );
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
        plugin_callback_entry_point: _,
    } = cli.command.operation(&env).attribute_to(actor);
    let clock_tick = is_clock_tick(&cli.command);
    let quiet_clock_tick =
        clock_tick && !matches!(sink.mode(), OutputMode::Json | OutputMode::Ndjson);
    // Invalid `orbit tool run` input cannot run. Reporting it before generation
    // pin keeps a typo from resolving a workspace or applying pending migrations.
    let unusable_tool_input = matches!(runtime_need, RuntimeNeed::UnusableToolInput { .. });
    // This prerequisite must fail without even a generation registration.
    // Successful rebinds still take normal executable-generation admission.
    if matches!(&cli.command, command::Commands::Workspace(workspace)
        if matches!(&workspace.command, command::workspace::WorkspaceSubcommand::Rebind(_)))
        && let Err(error) = resolved
            .generation_root()
            .and_then(orbit_cmd::workspace_rebind::require_checkout_rebind_host_store)
    {
        print_error(&error, &sink, json_error_preference);
        orbit_common::observability::logging::exit(1);
    }
    let _generation = if matches!(&cli.command, command::Commands::Update(_))
        || inspection
        || unusable_tool_input
    {
        None
    } else {
        let root = match resolved.process_generation_root() {
            Ok(root) => root,
            Err(error) => {
                print_error(&error, &sink, None);
                orbit_common::observability::logging::exit(1);
            }
        };
        let root_source = explicit_root_source(root_override.as_deref(), &env);
        let root_existed = root.exists();
        if let Some(source) = root_source
            && let Err(error) = root_check::validate_explicit_root(root, source)
        {
            print_error(&error, &sink, None);
            orbit_common::observability::logging::exit(1);
        }
        match pin_executable_generation_as(
            root,
            matches!(
                runtime_need,
                RuntimeNeed::ReadOnly
                    | RuntimeNeed::Inspection
                    | RuntimeNeed::PluginReadOnly
                    | RuntimeNeed::HostReadOnly
            ),
            participant_role(&cli.command),
            handover_capability(&cli.command),
        ) {
            Ok(guard) => {
                if clock_tick
                    && let Ok(digest) = orbit_common::fs::generation::process_generation(&env)
                    && let Ok(Some(summary)) =
                        orbit_common::fs::generation::finish_clock_generation_hold(
                            root,
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
                    report_refused_clock_tick(root, &error, &env);
                } else if clock_tick
                    && orbit_common::fs::generation::is_clock_generation_hold(&error)
                    && let Ok(digest) = orbit_common::fs::generation::process_generation(&env)
                    && orbit_common::fs::generation::record_clock_generation_hold(
                        root,
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
                        root_check::missing_root_error(root, source, &error)
                    }
                    _ => error,
                };
                print_error(&error, &sink, None);
                orbit_common::observability::logging::exit(1);
            }
        }
    };

    // A claimed worker addresses the owner's task by ID, but the follower's
    // task registry intentionally has no local copy. Composition reads the
    // worker binding from the host's protected invocation record and routes
    // its owner calls to the worker's bound checkout, so Core can carry them
    // over the authenticated run broker; request fields never select this
    // path. ToolRunArgs validates any explicit selector against that binding.
    let route = if unusable_tool_input {
        PreOpenRoute::Selected(workspace_selector.as_deref())
    } else {
        match resolved.route(
            &env,
            claimed_owner_tool(&cli.command),
            workspace_selector.as_deref(),
        ) {
            Ok(route) => route,
            Err(error) => {
                print_error(&error, &sink, json_error_preference);
                orbit_common::observability::logging::exit(1);
            }
        }
    };
    if let command::Commands::Config(config) = &cli.command
        && let command::config::ConfigSubcommand::Set(args) = &config.command
        && let Err(error) = command::config::set::preflight_indirect_workspace_write(
            root_override.as_deref(),
            args.global,
            workspace_selector.is_some(),
            &env,
        )
    {
        print_error(&error, &sink, json_error_preference);
        orbit_common::observability::logging::exit(1);
    }
    let bootstrapped = if let RuntimeNeed::UnusableToolInput { message } = &runtime_need {
        Err(orbit_common::OrbitError::InvalidInput(message.clone()))
    } else if let PreOpenRoute::Selected(selection) = route {
        match &runtime_need {
            RuntimeNeed::UnusableToolInput { message } => {
                Err(orbit_common::OrbitError::InvalidInput(message.clone()))
            }
            RuntimeNeed::Forbidden => {
                // A runtime-forbidden command has no store to audit against.
                let result = dispatch(
                    cli.command,
                    DispatchContext::without_runtime(
                        root_override.as_deref(),
                        workspace_selector.as_deref(),
                        &env,
                        &resolved,
                    ),
                );
                finish_command(result, &sink, json_error_preference, None);
                return;
            }
            RuntimeNeed::Required => RegisteredRuntimeFactory::initialize_with_overrides(
                root_override.as_deref(),
                selection,
                actor.clone(),
            ),
            RuntimeNeed::SelectedWorkspace { selector } => {
                RegisteredRuntimeFactory::initialize_with_overrides(
                    root_override.as_deref(),
                    Some(selector),
                    actor.clone(),
                )
            }
            RuntimeNeed::PipelineWorker => {
                RegisteredRuntimeFactory::initialize_pipeline_worker_with_overrides(
                    root_override.as_deref(),
                    selection,
                    actor.clone(),
                )
            }
            RuntimeNeed::ReadOnly => match task_owner_id.as_deref() {
                Some(task_id) => orbit_cmd::task_owner::initialize_for_task_show(
                    root_override.as_deref(),
                    selection,
                    task_id,
                    actor.clone(),
                ),
                None => RegisteredRuntimeFactory::initialize_read_only_with_overrides(
                    root_override.as_deref(),
                    selection,
                    actor.clone(),
                ),
            },
            RuntimeNeed::Inspection => {
                RegisteredRuntimeFactory::initialize_observational_with_overrides(
                    root_override.as_deref(),
                    selection,
                    actor.clone(),
                )
            }
            RuntimeNeed::PluginReadOnly => {
                RegisteredRuntimeFactory::initialize_plugin_read_only_with_overrides(
                    root_override.as_deref(),
                    selection,
                    actor.clone(),
                )
            }
            RuntimeNeed::HostReadOnly => {
                RegisteredRuntimeFactory::initialize_host_read_only_with_overrides(
                    root_override.as_deref(),
                    selection,
                    actor.clone(),
                )
            }
            RuntimeNeed::TaskOwner { task_id } => orbit_cmd::task_owner::initialize_for_task_show(
                root_override.as_deref(),
                selection,
                task_id,
                actor.clone(),
            ),
        }
    } else {
        // The claimed-owner route opens the worker's bound checkout.
        RegisteredRuntimeFactory::initialize_with_overrides(
            root_override.as_deref(),
            None,
            actor.clone(),
        )
    };

    // The opaque handle over this command's workspace, for a command that
    // runs through an app's `exec`.
    let core = match bootstrapped {
        Ok(core) => core,
        Err(err) => {
            // `orbit doctor` reports a runtime that would not open instead of
            // failing with the bare refusal when the cause is a misplaced
            // host `[plugin_enablement]` table (read from the file), a store
            // that was never created, or a store that needs a writable
            // migration. Everything else fails closed.
            if let command::Commands::Doctor(doctor) = &cli.command
                && let Some(degraded) = doctor.degraded_report(root_override.as_deref(), &err)
            {
                finish_command(degraded, &sink, json_error_preference, None);
                return;
            }
            print_error(&err, &sink, json_error_preference);
            orbit_common::observability::logging::exit(1);
        }
    };
    let context = DispatchContext::with_core(
        &core,
        root_override.as_deref(),
        workspace_selector.as_deref(),
        &env,
        &resolved,
    );
    // An app's `exec` authorizes and audits the command. The scope settles
    // that row once rendering finishes, and writes the row of an invocation
    // that never reached an `exec`.
    let audit = audit_meta.map(|meta| InvocationAuditScope::enter(&core, meta));
    let result = dispatch(cli.command, context);

    finish_command(result, &sink, json_error_preference, audit);
}
