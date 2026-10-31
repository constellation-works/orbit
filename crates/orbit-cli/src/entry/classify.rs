use orbit_common::fs::generation::ParticipantRole;

use crate::command;

/// Long-lived commands that must reap JSONL archives even when the active
/// file is within budget. `--help` never reaches here: clap exits in
/// [`super::discover::parse_cli`]. Short-lived commands rotate only if a
/// later JSONL write finds an oversized active file (one `metadata()` check,
/// no directory walk).
pub(super) fn command_rotates_jsonl_on_start(command: &command::Commands) -> bool {
    match command {
        command::Commands::Mcp(mcp) => matches!(
            mcp.command,
            command::mcp::McpSubcommand::Serve(_) | command::mcp::McpSubcommand::Listen(_)
        ),
        command::Commands::Web(web) => {
            matches!(web.command, command::web::WebSubcommand::Serve(_))
        }
        command::Commands::Sweep(_) => true,
        command::Commands::Clock(clock) => {
            matches!(clock.command, command::clock::ClockSubcommand::Tick(_))
        }
        _ => false,
    }
}

/// What this process is while it participates in upgrade admission, so a
/// breaking upgrade can name it and long-lived processes know to yield.
pub(super) fn participant_role(command: &command::Commands) -> ParticipantRole {
    use command::Commands;
    match command {
        Commands::Mcp(mcp) => match mcp.command {
            command::mcp::McpSubcommand::Serve(_) => ParticipantRole::McpServe,
            command::mcp::McpSubcommand::Listen(_) => ParticipantRole::McpListen,
            _ => ParticipantRole::Command,
        },
        Commands::Web(web) if matches!(web.command, command::web::WebSubcommand::Serve(_)) => {
            ParticipantRole::Dashboard
        }
        Commands::Job(job)
            if matches!(
                job.command,
                command::job::JobSubcommand::RunPipelineWorker(_)
            ) =>
        {
            ParticipantRole::Drain
        }
        command if is_clock_tick(command) => ParticipantRole::Clock,
        _ => ParticipantRole::Command,
    }
}

/// The resume capability this process hands over with when a candidate is
/// renamed over its executable: a stdio `mcp serve` whose stdin can be
/// polled, which is what the session needs to hand over without losing
/// input. A remote proxy relays another host's session and never does.
pub(super) fn handover_capability(command: &command::Commands) -> Option<&'static str> {
    match command {
        command::Commands::Mcp(mcp) => match &mcp.command {
            command::mcp::McpSubcommand::Serve(serve)
                if !matches!(serve.mode, Some(command::mcp::ServeMode::Remote))
                    && orbit_mcp::stdin_supports_handover() =>
            {
                Some(orbit_common::fs::generation::RESUME_MCP_STDIO)
            }
            _ => None,
        },
        _ => None,
    }
}
pub(super) fn is_clock_tick(command: &command::Commands) -> bool {
    matches!(command, command::Commands::Sweep(_))
        || matches!(command, command::Commands::Clock(clock)
            if matches!(clock.command, command::clock::ClockSubcommand::Tick(_)))
}

/// A clock tick refused because a breaking migration cannot run beside the
/// live processes stops routines and worktree GC on this host until they
/// exit. Say so on every refused tick, and keep the run of refusals for
/// `orbit doctor` [ORB-15260].
pub(super) fn report_refused_clock_tick(root: &std::path::Path, error: &orbit_core::OrbitError) {
    let recorded = orbit_common::fs::generation::process_generation().and_then(|digest| {
        orbit_common::fs::generation::record_clock_upgrade_refusal(
            root,
            digest,
            chrono::Utc::now(),
            error,
        )
    });
    let run = match &recorded {
        Ok(hold) => format!(
            "{} tick(s) in a row since {}",
            hold.refused_ticks,
            hold.started_at.to_rfc3339()
        ),
        Err(record_error) => format!("the refusal could not be recorded: {record_error}"),
    };
    tracing::error!(
        target: "orbit.cli.clock",
        error = %error,
        "clock tick refused by upgrade admission"
    );
    eprintln!(
        "clock tick refused by upgrade admission ({run}): scheduled routines and worktree GC \
         are not running on this host until the live Orbit processes named below exit or \
         yield; `orbit doctor` reports this hold"
    );
}

/// Whether the public tool CLI is asking for one of a claimed worker's owner
/// calls. The tool name only selects the bootstrap path; the worker's
/// protected invocation record supplies and limits its authority.
pub(super) fn claimed_owner_tool(command: &command::Commands) -> bool {
    let command::Commands::Tool(tool) = command else {
        return false;
    };
    let command::tool::ToolSubcommand::Run(args) = &tool.command else {
        return false;
    };
    orbit_types::tool::is_claimed_owner_tool(&args.name)
}
