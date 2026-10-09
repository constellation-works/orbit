use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use orbit_cmd::agent_rules::{InjectionAction, InjectionOutcome, inject_agent_rules};
use orbit_core::OrbitError;
use orbit_types::workspace::WorkspaceCheckoutRole;
use serde_json::{Value, json};

use super::super::support::{manages_checkout_local_orbit_files, orbit_gitignore_path};
use super::WorkspaceInitResult;

const ONBOARDING_FINALIZE_GUIDANCE: &str = "review the managed `.gitignore` entry (Orbit ignores `.orbit/` as per-user state and does not auto-commit or discard operator changes)";
const RELOCATED_ROOT_ONBOARDING_GUIDANCE: &str = "review generated Orbit definitions in the configured Orbit root before local workflows (Orbit does not auto-commit or discard operator changes)";

fn onboarding_finalize_guidance(workspace_root: &Path, orbit_dir: &Path) -> &'static str {
    if manages_checkout_local_orbit_files(workspace_root, orbit_dir) {
        ONBOARDING_FINALIZE_GUIDANCE
    } else {
        RELOCATED_ROOT_ONBOARDING_GUIDANCE
    }
}

fn render_task_id_start(task_prefix: Option<&str>, next: u32) -> String {
    match task_prefix {
        Some(task_prefix) => format!("{task_prefix}-{next:05}"),
        None => format!("{next:05}"),
    }
}

pub(super) struct WorkspaceInitReport {
    id: String,
    name: String,
    root: PathBuf,
    orbit_dir: PathBuf,
    role: Option<WorkspaceCheckoutRole>,
    owner_machine_id: Option<String>,
    /// A replica's owner as this machine registered it with `orbit host add`,
    /// or `None` while the owner has no host entry. Never added here.
    owner_host: Option<String>,
    onboarding: &'static str,
    checkout_files: Vec<String>,
    allocator: AllocatorOutcome,
    mcp: McpOutcome,
    rules: RulesOutcome,
}

enum AllocatorOutcome {
    Skipped,
    Ran { next: String, changed: bool },
}

enum McpOutcome {
    Skipped,
    NoneDetected,
    Configured(Vec<String>),
}

enum RulesOutcome {
    Skipped,
    Injected(Vec<RuleFileOutcome>),
}

struct RuleFileOutcome {
    label: String,
    action: InjectionAction,
}

pub(super) fn collect_init_report(
    init_result: WorkspaceInitResult,
    global_root: &Path,
    task_id_start: Option<u32>,
    mcp: bool,
    inject_rules: bool,
) -> Result<WorkspaceInitReport, OrbitError> {
    let onboarding = onboarding_finalize_guidance(&init_result.root, &init_result.orbit_dir);
    let mut checkout_files = BTreeSet::new();
    if let Some(gitignore) = orbit_gitignore_path(&init_result.root, &init_result.orbit_dir) {
        checkout_files.insert(checkout_file_label(&init_result.root, &gitignore));
    }

    let allocator = match task_id_start {
        Some(start) => {
            let outcome = orbit_core::bootstrap::task_migration::seed_task_id_start(
                global_root,
                init_result.task_prefix.as_deref(),
                start,
            )?;
            AllocatorOutcome::Ran {
                next: render_task_id_start(init_result.task_prefix.as_deref(), outcome.next),
                changed: outcome.changed,
            }
        }
        None => AllocatorOutcome::Skipped,
    };

    let mcp_outcome = if mcp {
        let (providers, files) = crate::command::mcp::init_auto_for_workspace(
            &init_result.root,
            &init_result.orbit_dir,
            &init_result.id,
        )?;
        for file in files {
            checkout_files.insert(checkout_file_label(&init_result.root, &file));
        }
        if providers.is_empty() {
            McpOutcome::NoneDetected
        } else {
            McpOutcome::Configured(providers)
        }
    } else {
        McpOutcome::Skipped
    };

    let rules_outcome = if inject_rules {
        let outcome = inject_agent_rules(&init_result.root)?;
        for entry in &outcome.outcomes {
            checkout_files.insert(checkout_file_label(&init_result.root, &entry.path));
        }
        RulesOutcome::Injected(
            outcome
                .outcomes
                .into_iter()
                .map(rule_file_outcome)
                .collect(),
        )
    } else {
        RulesOutcome::Skipped
    };

    let owner_host = match (init_result.role, init_result.owner_machine_id.as_deref()) {
        (Some(WorkspaceCheckoutRole::Replica), Some(owner)) => registered_owner(global_root, owner),
        _ => None,
    };
    Ok(WorkspaceInitReport {
        id: init_result.id,
        name: init_result.name,
        root: init_result.root,
        orbit_dir: init_result.orbit_dir,
        role: init_result.role,
        owner_machine_id: init_result.owner_machine_id,
        owner_host,
        onboarding,
        checkout_files: checkout_files.into_iter().collect(),
        allocator,
        mcp: mcp_outcome,
        rules: rules_outcome,
    })
}

/// The host name a replica's owner is registered under, if it is.
fn registered_owner(global_root: &Path, owner_machine_id: &str) -> Option<String> {
    let registry = orbit_registry::hosts::load_host_registry(global_root).ok()?;
    match registry.resolve(owner_machine_id).ok()? {
        orbit_registry::hosts::ResolvedHost::Entry(entry) => Some(entry.name.clone()),
        orbit_registry::hosts::ResolvedHost::Legacy(row) => Some(row.ssh.clone()),
        orbit_registry::hosts::ResolvedHost::Local(_) => None,
    }
}

fn checkout_file_label(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn rule_file_outcome(entry: InjectionOutcome) -> RuleFileOutcome {
    RuleFileOutcome {
        label: entry
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| entry.path.display().to_string()),
        action: entry.action,
    }
}

pub(super) fn workspace_init_json(report: &WorkspaceInitReport) -> Value {
    json!({
        "id": report.id,
        "name": report.name,
        "root": report.root.to_string_lossy(),
        "orbit_dir": report.orbit_dir.to_string_lossy(),
        "role": report.role.map(|role| role.to_string()),
        "owner_machine_id": report.owner_machine_id,
        "owner_host": report.owner_host,
        "onboarding": report.onboarding,
        "checkout_files": report.checkout_files,
        "before_ship": if report.checkout_files.is_empty() { None } else { Some("Review and commit the listed checkout files before shipping; local delivery refuses tracked changes, merge conflicts, and untracked paths overlapping the incoming changes.") },
        "allocator": allocator_json(&report.allocator),
        "mcp": mcp_json(&report.mcp),
        "rules": rules_json(&report.rules),
    })
}

fn allocator_json(outcome: &AllocatorOutcome) -> Value {
    match outcome {
        AllocatorOutcome::Skipped => json!({
            "status": "skipped",
            "next": null,
            "changed": null,
        }),
        AllocatorOutcome::Ran { next, changed } => json!({
            "status": if *changed { "seeded" } else { "unchanged" },
            "next": next,
            "changed": changed,
        }),
    }
}

fn mcp_json(outcome: &McpOutcome) -> Value {
    match outcome {
        McpOutcome::Skipped => json!({
            "status": "skipped",
            "providers": null,
        }),
        McpOutcome::NoneDetected => json!({
            "status": "none_detected",
            "providers": [],
        }),
        McpOutcome::Configured(providers) => json!({
            "status": "configured",
            "providers": providers,
        }),
    }
}

fn rules_json(outcome: &RulesOutcome) -> Value {
    match outcome {
        RulesOutcome::Skipped => json!({
            "status": "skipped",
            "outcomes": null,
        }),
        RulesOutcome::Injected(entries) => json!({
            "status": "injected",
            "outcomes": entries
                .iter()
                .map(|entry| json!({
                    "file": entry.label,
                    "action": injection_action_token(&entry.action),
                }))
                .collect::<Vec<_>>(),
        }),
    }
}

fn injection_action_token(action: &InjectionAction) -> &'static str {
    match action {
        InjectionAction::Created => "created",
        InjectionAction::AppendedBlock => "appended",
        InjectionAction::ReplacedBlock => "replaced",
    }
}

pub(super) fn format_workspace_init(report: &WorkspaceInitReport) -> String {
    let mut lines = vec![
        format!("workspace '{}' initialized", report.name),
        format!("  id:        {}", report.id),
        format!("  root:      {}", report.root.display()),
        format!("  orbit_dir: {}", report.orbit_dir.display()),
    ];
    if let Some(role) = report.role {
        lines.push(format!("  role:      {role}"));
    }
    if let Some(owner) = report.owner_machine_id.as_deref() {
        lines.push(format!("  owner:     {owner}"));
    }
    lines.push(format!("  onboarding: {}", report.onboarding));
    if report.role == Some(WorkspaceCheckoutRole::Replica) {
        lines.push(match (&report.owner_host, report.owner_machine_id.as_deref()) {
            (Some(host), _) => format!(
                "  next:      a replica executes through pull from its owner, registered here as \
                 '{host}': run `orbit run auto --pull <selector>`"
            ),
            (None, owner) => format!(
                "  next:      a replica executes through pull, and owner {} has no host entry \
                 here: register it with `orbit host add <ssh-target>`, then run \
                 `orbit run auto --pull <selector>`",
                owner.unwrap_or("(unknown)")
            ),
        });
    }
    if !report.checkout_files.is_empty() {
        lines.push("  checkout files written:".to_string());
        for file in &report.checkout_files {
            lines.push(format!("    {file}"));
        }
        lines.push("  before ship: review and commit these files; local delivery refuses tracked changes, merge conflicts, and untracked paths overlapping the incoming changes".to_string());
    }
    match &report.allocator {
        AllocatorOutcome::Skipped => {}
        AllocatorOutcome::Ran {
            next,
            changed: true,
        } => lines.push(format!("  id_start:  allocator seeded to {next}")),
        AllocatorOutcome::Ran {
            next,
            changed: false,
        } => lines.push(format!(
            "  id_start:  allocator already at {next} (unchanged)"
        )),
    }
    match &report.mcp {
        McpOutcome::Skipped => {
            lines.push("  mcp:       skipped (pass --mcp to set up integrations)".to_string());
        }
        McpOutcome::NoneDetected => {
            lines.push("  mcp:       no providers auto-detected".to_string());
        }
        McpOutcome::Configured(providers) => lines.push(format!(
            "  mcp:       {} (operator-authorized: orbit.workflow.ship, run observe/resume, orbit.command.exec)",
            providers.join(", ")
        )),
    }
    if let RulesOutcome::Injected(entries) = &report.rules {
        for entry in entries {
            let verb = match entry.action {
                InjectionAction::Created => "created with Orbit rules block",
                InjectionAction::AppendedBlock => "Orbit rules block appended",
                InjectionAction::ReplacedBlock => "Orbit rules block refreshed",
            };
            lines.push(format!("  rules:     {}: {verb}", entry.label));
        }
    }
    lines.join("\n")
}
