use std::fs;
use std::path::{Path, PathBuf};

use orbit_cmd::mcp_clients::{self, require_home_dir, vscode_home_user_dir};
use orbit_core::OrbitError;

use crate::command::mcp::ORBIT_MCP_SERVER_ID;
use crate::command::{CommandOutput, Payload};

use super::args::{McpAction, McpProvider, ProviderSelectionMode, ScopeArg};
use super::format::{load_toml_document, write_or_remove_toml_document};
use super::providers::*;

fn server_id_for_action(action: McpAction<'_>) -> &'static str {
    match action {
        McpAction::Init(ServerLaunch::Federated) | McpAction::RemoveFederated => {
            ORBIT_FEDERATED_MCP_SERVER_ID
        }
        McpAction::Init(ServerLaunch::Local { .. }) | McpAction::Remove => ORBIT_MCP_SERVER_ID,
    }
}

fn shared_grok_target(target: &ConfigTarget) -> ConfigTarget {
    ConfigTarget {
        mcp_path: target.mcp_path.clone(),
        legacy_mcp_path: None,
        settings_path: None,
        scope: target.scope,
    }
}

/// Remove only the exact Grok TOML shape emitted by older Orbit versions.
/// A user's own server with the same name but a different launch or extra
/// settings is left alone, since Orbit has no ownership marker in old files.
fn cleanup_legacy_grok_path(target: &ConfigTarget, server_id: &str) -> Result<(), OrbitError> {
    let Some(path) = target.legacy_mcp_path.as_ref().filter(|path| path.exists()) else {
        return Ok(());
    };
    let mut doc = load_toml_document(path)?;
    if doc.to_string().trim().is_empty() {
        return Ok(());
    }
    if doc
        .get("mcp_servers")
        .and_then(toml_edit::Item::as_table_like)
        .and_then(|servers| servers.get(server_id))
        .is_none()
    {
        return Ok(());
    }
    let parsed: toml::Value = doc.to_string().parse().map_err(|err| {
        OrbitError::InvalidInput(format!("invalid TOML '{}': {err}", path.display()))
    })?;
    let owned = parsed
        .get("mcp_servers")
        .and_then(|servers| servers.get(server_id))
        .is_some_and(|entry| is_legacy_orbit_grok_entry(entry, server_id));
    if !owned {
        return Ok(());
    }
    if let Some(servers) = doc
        .get_mut("mcp_servers")
        .and_then(toml_edit::Item::as_table_like_mut)
    {
        servers.remove(server_id);
        if servers.is_empty() {
            doc.remove("mcp_servers");
        }
    }
    write_or_remove_toml_document(path, &doc)?;
    if !path.exists()
        && let Some(parent) = path.parent()
        && parent
            .read_dir()
            .map_err(|error| {
                OrbitError::Io(format!("failed to read '{}': {error}", parent.display()))
            })?
            .next()
            .is_none()
    {
        fs::remove_dir(parent).map_err(|error| {
            OrbitError::Io(format!("failed to remove '{}': {error}", parent.display()))
        })?;
    }
    Ok(())
}

fn is_legacy_orbit_grok_entry(entry: &toml::Value, server_id: &str) -> bool {
    let Some(table) = entry.as_table() else {
        return false;
    };
    if table.len() != 3
        || table.get("command").and_then(toml::Value::as_str) != Some("orbit")
        || table.get("enabled").and_then(toml::Value::as_bool) != Some(true)
    {
        return false;
    }
    let Some(args) = table.get("args").and_then(toml::Value::as_array) else {
        return false;
    };
    let args = args
        .iter()
        .map(toml::Value::as_str)
        .collect::<Option<Vec<_>>>();
    let Some(args) = args else {
        return false;
    };
    match (server_id, args.as_slice()) {
        (ORBIT_MCP_SERVER_ID, ["mcp", "serve"])
        | (ORBIT_MCP_SERVER_ID, ["mcp", "serve", "--operator"])
        | (ORBIT_FEDERATED_MCP_SERVER_ID, ["mcp", "serve", "--mode", "federated"]) => true,
        (ORBIT_MCP_SERVER_ID, ["mcp", "serve", "--workspace", workspace])
        | (ORBIT_MCP_SERVER_ID, ["mcp", "serve", "--operator", "--workspace", workspace]) => {
            !workspace.is_empty()
        }
        _ => false,
    }
}

pub(super) fn run_action(
    action: McpAction<'_>,
    repo_root: &Path,
    orbit_root: &Path,
    selection: ProviderSelectionMode,
    home_dir: Option<PathBuf>,
    scope: ScopeArg,
) -> Result<Vec<McpProvider>, OrbitError> {
    let providers = resolve_providers(selection, repo_root, home_dir.as_deref());
    for provider in &providers {
        let target = ConfigTarget::resolve(scope, provider, repo_root, home_dir.as_deref())?;
        match action {
            McpAction::Init(launch) => match provider {
                McpProvider::Claude => apply_claude_init(&target, launch)?,
                McpProvider::Codex => apply_toml_init(&target, launch, true)?,
                McpProvider::Gemini => apply_gemini_init(&target, launch)?,
                McpProvider::Antigravity => apply_simple_json_init(&target, "mcpServers", launch)?,
                McpProvider::Grok => {
                    if target.legacy_mcp_path.is_some() {
                        // Claude's handler provides the ~/.claude.json lock.
                        // When Claude is selected it owns the shared write.
                        if !providers.contains(&McpProvider::Claude) {
                            apply_claude_init(&shared_grok_target(&target), launch)?;
                        }
                        cleanup_legacy_grok_path(&target, server_id_for_action(action))?;
                    } else {
                        apply_toml_init(&target, launch, false)?;
                    }
                }
                McpProvider::Cursor => apply_simple_json_init(&target, "mcpServers", launch)?,
                McpProvider::Vscode => apply_simple_json_init(&target, "servers", launch)?,
                McpProvider::Windsurf => apply_simple_json_init(&target, "mcpServers", launch)?,
            },
            McpAction::Remove | McpAction::RemoveFederated => {
                let server_id = if matches!(action, McpAction::RemoveFederated) {
                    ORBIT_FEDERATED_MCP_SERVER_ID
                } else {
                    ORBIT_MCP_SERVER_ID
                };
                match provider {
                    McpProvider::Claude => apply_claude_remove(&target, server_id)?,
                    McpProvider::Codex => apply_toml_remove(&target, server_id)?,
                    McpProvider::Gemini => apply_gemini_remove(&target, server_id)?,
                    McpProvider::Antigravity => {
                        apply_simple_json_remove(&target, "mcpServers", server_id)?
                    }
                    McpProvider::Grok => {
                        if target.legacy_mcp_path.is_some() {
                            apply_claude_remove(&shared_grok_target(&target), server_id)?;
                            cleanup_legacy_grok_path(&target, server_id)?;
                        } else {
                            apply_toml_remove(&target, server_id)?;
                        }
                    }
                    McpProvider::Cursor => {
                        apply_simple_json_remove(&target, "mcpServers", server_id)?
                    }
                    McpProvider::Vscode => apply_simple_json_remove(&target, "servers", server_id)?,
                    McpProvider::Windsurf => {
                        apply_simple_json_remove(&target, "mcpServers", server_id)?
                    }
                }
            }
        }
    }
    let _ = orbit_root;
    Ok(providers)
}

/// Resolved file targets for a single provider+scope.
///
/// `mcp_path` is the active registry, `legacy_mcp_path` is a migration source,
/// and `settings_path` is Claude's optional permissions file. Scope determines
/// whether they live in HOME or in the repo.
pub(super) struct ConfigTarget {
    pub(super) mcp_path: PathBuf,
    pub(super) legacy_mcp_path: Option<PathBuf>,
    pub(super) settings_path: Option<PathBuf>,
    pub(super) scope: ScopeArg,
}

impl ConfigTarget {
    pub(super) fn resolve(
        scope: ScopeArg,
        provider: &McpProvider,
        repo_root: &Path,
        home_dir: Option<&Path>,
    ) -> Result<Self, OrbitError> {
        let paths = mcp_clients::client_config_paths(
            scope.client_scope(),
            provider.client(),
            repo_root,
            home_dir,
        )?;
        Ok(Self {
            mcp_path: paths.mcp_path,
            legacy_mcp_path: paths.legacy_mcp_path,
            settings_path: paths.settings_path,
            scope,
        })
    }
}

fn resolve_providers(
    selection: ProviderSelectionMode,
    repo_root: &Path,
    home_dir: Option<&Path>,
) -> Vec<McpProvider> {
    match selection {
        ProviderSelectionMode::Explicit(providers) => providers,
        ProviderSelectionMode::Auto => auto_detected_providers(repo_root, home_dir),
    }
}

pub(super) fn auto_detected_providers(
    repo_root: &Path,
    home_dir: Option<&Path>,
) -> Vec<McpProvider> {
    let mut providers = Vec::new();
    let claude_repo = repo_root.join(".claude").is_dir();
    // A bare `~/.claude` directory is not evidence that Claude Code is
    // installed: `orbit init` creates `~/.claude/skills/` for its own skill
    // links (`orbit_core::bootstrap::init`), so a directory test would detect
    // Claude on every host Orbit has ever touched. Require a config file
    // Claude Code itself writes, the way every other home probe below does.
    let claude_home = home_dir
        .map(|home| {
            home.join(".claude.json").is_file()
                || home.join(".claude").join("settings.json").is_file()
        })
        .unwrap_or(false);
    if claude_repo || claude_home {
        providers.push(McpProvider::Claude);
    }
    if home_dir
        .map(|home| home.join(".codex").join("config.toml").is_file())
        .unwrap_or(false)
    {
        providers.push(McpProvider::Codex);
    }
    let gemini_repo = repo_root.join(".gemini").is_dir();
    let gemini_home = home_dir
        .map(|home| home.join(".gemini").join("settings.json").is_file())
        .unwrap_or(false);
    if gemini_repo || gemini_home {
        providers.push(McpProvider::Gemini);
    }
    let antigravity_repo = repo_root.join(".agents").join("mcp_config.json").is_file();
    let antigravity_home = home_dir
        .map(|home| {
            home.join(".gemini")
                .join("config")
                .join("mcp_config.json")
                .is_file()
                || home.join(".gemini").join("antigravity-cli").is_dir()
        })
        .unwrap_or(false);
    if antigravity_repo || antigravity_home {
        providers.push(McpProvider::Antigravity);
    }
    let grok_repo = repo_root.join(".grok").is_dir();
    let grok_home = home_dir
        .map(|home| home.join(".grok").join("config.toml").is_file())
        .unwrap_or(false);
    if grok_repo || grok_home {
        providers.push(McpProvider::Grok);
    }
    let cursor_repo = repo_root.join(".cursor").is_dir();
    let cursor_home = home_dir
        .map(|home| home.join(".cursor").join("mcp.json").is_file())
        .unwrap_or(false);
    if cursor_repo || cursor_home {
        providers.push(McpProvider::Cursor);
    }
    let vscode_repo = repo_root.join(".vscode").is_dir();
    let vscode_home = home_dir
        .map(|home| vscode_home_user_dir(home).join("mcp.json").is_file())
        .unwrap_or(false);
    if vscode_repo || vscode_home {
        providers.push(McpProvider::Vscode);
    }
    let windsurf_home = home_dir
        .map(|home| {
            home.join(".codeium")
                .join("windsurf")
                .join("mcp_config.json")
                .is_file()
        })
        .unwrap_or(false);
    if windsurf_home {
        providers.push(McpProvider::Windsurf);
    }
    providers
}

/// Print which providers were touched and, crucially, where — resolution now
/// goes through the workspace registry (ORB-12121) rather than always
/// matching cwd, so the write can land in a linked worktree's primary
/// checkout or in a `--root`-selected checkout the operator isn't standing
/// in. Naming the path (and, when known, the bound `ws_*` id) turns that
/// into an audit line instead of a silent no-op.
///
/// Workspace scope writes land under `repo_root`, so naming it is accurate.
/// Home scope (ORB-12139) writes under `home_dir` instead — each provider's
/// `ConfigTarget::resolve` output is named there so the line still points at
/// the file the run actually touched, not the unrelated checkout.
pub(super) fn action_payload(
    action: McpAction<'_>,
    providers: &[McpProvider],
    repo_root: &Path,
    home_dir: Option<&Path>,
    scope: ScopeArg,
    workspace_id: Option<&str>,
) -> Result<CommandOutput, OrbitError> {
    let text = format_action_summary(action, providers, repo_root, home_dir, scope, workspace_id)?;
    let scope_label = match scope {
        ScopeArg::Workspace => "workspace",
        ScopeArg::Home => "home",
    };
    let doc = serde_json::json!({
        "action": action.label(),
        "scope": scope_label,
        "workspace_id": workspace_id,
        "repo_root": repo_root.display().to_string(),
        "providers": providers.iter().map(|provider| provider.label()).collect::<Vec<_>>(),
    });
    Ok(Payload::detail(doc, text).into())
}

#[allow(clippy::too_many_arguments)]
fn format_action_summary(
    action: McpAction<'_>,
    providers: &[McpProvider],
    repo_root: &Path,
    home_dir: Option<&Path>,
    scope: ScopeArg,
    workspace_id: Option<&str>,
) -> Result<String, OrbitError> {
    if providers.is_empty() {
        return Ok(format!("mcp {}: no providers selected", action.label()));
    }

    match scope {
        ScopeArg::Workspace => {
            let labels = providers
                .iter()
                .map(|provider| provider.label())
                .collect::<Vec<_>>()
                .join(", ");
            Ok(match workspace_id {
                Some(id) => format!(
                    "mcp {}: {} -> {} ({})",
                    action.label(),
                    labels,
                    repo_root.display(),
                    id
                ),
                None => format!(
                    "mcp {}: {} -> {}",
                    action.label(),
                    labels,
                    repo_root.display()
                ),
            })
        }
        ScopeArg::Home => {
            // `run_action` already succeeded with this scope, which requires
            // `require_home_dir` to have resolved a home directory; resolving
            // it again here for display should not hit the missing-HOME case,
            // but the error is still propagated rather than assumed away.
            let home = require_home_dir(home_dir)?;
            let mut targets = Vec::with_capacity(providers.len());
            for provider in providers {
                let target = ConfigTarget::resolve(scope, provider, repo_root, Some(home))?;
                targets.push(format!(
                    "{} -> {}",
                    provider.label(),
                    target.mcp_path.display()
                ));
            }
            let targets = targets.join(", ");
            Ok(match workspace_id {
                Some(id) => format!("mcp {}: {} (bound to {})", action.label(), targets, id),
                None => format!("mcp {}: {}", action.label(), targets),
            })
        }
    }
}
