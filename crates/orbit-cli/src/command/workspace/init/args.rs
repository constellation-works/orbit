use clap::Args;

use super::super::role::CliCheckoutRole;

#[derive(Args)]
pub struct WorkspaceInitArgs {
    /// Workspace name (defaults to directory name)
    #[arg(long)]
    pub name: Option<String>,
    /// Base branch for this workspace (default: the checked-out branch, or main)
    ///
    /// Kept optional so re-initializing an existing workspace can distinguish
    /// an omitted value from an explicit request to reset it to `main`.
    #[arg(long)]
    pub base_branch: Option<String>,
    /// Ship-pipeline mode for this workspace: `pr` or `local`. When omitted, the
    /// effective mode defaults to `pr`; pass `--ship-mode local` for in-place delivery.
    #[arg(long, value_name = "MODE")]
    pub ship_mode: Option<String>,
    /// Explicit local checkout role. Omit for the compatible local-owner
    /// default; use `replica --owner hm_...` to bootstrap a replica atomically.
    #[arg(long, value_enum)]
    pub role: Option<CliCheckoutRole>,
    /// Stable owner machine_id. Required with `--role replica` and rejected
    /// for the local-owner role.
    #[arg(long)]
    pub owner: Option<String>,
    /// Seed the local task-id allocator so the next task id is N (e.g. hand this
    /// machine a disjoint id range like 10000+). The counter only moves forward;
    /// a value below the current position is refused.
    #[arg(long, value_name = "N")]
    pub task_id_start: Option<u32>,
    /// Set up MCP client integrations for auto-detected providers. The
    /// registered server is granted OPERATOR authority: governed operations
    /// such as `orbit.workflow.ship`, workflow run observation/resume, and
    /// `orbit.command.exec` become reachable through it. Bare `orbit mcp
    /// serve` and worker/agent MCP startup remain agent-only.
    #[arg(long)]
    pub mcp: bool,
    /// Inject (or refresh) an Orbit workflow-rules block in CLAUDE.md and AGENTS.md at the workspace root.
    #[arg(long)]
    pub inject_agent_rules: bool,
    /// No-op (kept for backwards compatibility — defaults are always refreshed on init)
    #[arg(long, hide = true)]
    pub refresh_defaults: bool,
    /// Reconcile an already registered workspace after validating its logical
    /// and checkout binding. A missing or malformed identity is restored only
    /// for that exact binding; malformed bytes are archived first. Also
    /// replaces a checkout identity that no registration claims. On the declared
    /// owner's checkout, records a first source identity from a portable Git
    /// origin. An existing source identity requires `workspace source-remote
    /// rebind` to change.
    #[arg(long)]
    pub force: bool,
}
