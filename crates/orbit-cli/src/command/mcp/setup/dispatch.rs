use std::path::{Path, PathBuf};

use orbit_core::OrbitError;

use crate::command::mcp::ORBIT_MCP_SERVER_ID;

use super::args::{McpAction, McpProvider, ProviderSelectionMode, ScopeArg};
use super::providers::*;

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
                McpProvider::Codex => apply_codex_init(&target, launch)?,
                McpProvider::Gemini => apply_gemini_init(&target, launch)?,
                McpProvider::Antigravity => apply_simple_json_init(&target, "mcpServers", launch)?,
                McpProvider::Grok => apply_grok_init(&target, launch)?,
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
                    McpProvider::Codex => apply_codex_remove(&target, server_id)?,
                    McpProvider::Gemini => apply_gemini_remove(&target, server_id)?,
                    McpProvider::Antigravity => {
                        apply_simple_json_remove(&target, "mcpServers", server_id)?
                    }
                    McpProvider::Grok => apply_grok_remove(&target, server_id)?,
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
/// Each provider has at most two writable files: the MCP server registry
/// (`mcp_path`) and an optional permissions/settings file (`settings_path`,
/// only used by Claude today). Scope determines whether they live in HOME
/// or in the repo.
pub(super) struct ConfigTarget {
    pub(super) mcp_path: PathBuf,
    pub(super) settings_path: Option<PathBuf>,
}

impl ConfigTarget {
    fn resolve(
        scope: ScopeArg,
        provider: &McpProvider,
        repo_root: &Path,
        home_dir: Option<&Path>,
    ) -> Result<Self, OrbitError> {
        match (scope, provider) {
            (ScopeArg::Home, McpProvider::Claude) => {
                let home = require_home_dir(home_dir)?;
                Ok(Self {
                    mcp_path: home.join(".claude").join(".mcp.json"),
                    settings_path: Some(home.join(".claude").join("settings.json")),
                })
            }
            (ScopeArg::Workspace, McpProvider::Claude) => Ok(Self {
                mcp_path: repo_root.join(".claude.json"),
                settings_path: Some(repo_root.join(".claude").join("settings.json")),
            }),
            (ScopeArg::Home, McpProvider::Codex) => {
                let home = require_home_dir(home_dir)?;
                Ok(Self {
                    mcp_path: home.join(".codex").join("config.toml"),
                    settings_path: None,
                })
            }
            (ScopeArg::Workspace, McpProvider::Codex) => Ok(Self {
                mcp_path: repo_root.join(".codex").join("config.toml"),
                settings_path: None,
            }),
            (ScopeArg::Home, McpProvider::Gemini) => {
                let home = require_home_dir(home_dir)?;
                Ok(Self {
                    mcp_path: home.join(".gemini").join("settings.json"),
                    settings_path: None,
                })
            }
            (ScopeArg::Workspace, McpProvider::Gemini) => Ok(Self {
                mcp_path: repo_root.join(".gemini").join("settings.json"),
                settings_path: None,
            }),
            (ScopeArg::Home, McpProvider::Antigravity) => {
                let home = require_home_dir(home_dir)?;
                Ok(Self {
                    mcp_path: home.join(".gemini").join("config").join("mcp_config.json"),
                    settings_path: None,
                })
            }
            (ScopeArg::Workspace, McpProvider::Antigravity) => Ok(Self {
                mcp_path: repo_root.join(".agents").join("mcp_config.json"),
                settings_path: None,
            }),
            (ScopeArg::Home, McpProvider::Grok) => {
                let home = require_home_dir(home_dir)?;
                Ok(Self {
                    mcp_path: home.join(".grok").join("config.toml"),
                    settings_path: None,
                })
            }
            (ScopeArg::Workspace, McpProvider::Grok) => Ok(Self {
                mcp_path: repo_root.join(".grok").join("config.toml"),
                settings_path: None,
            }),
            (ScopeArg::Home, McpProvider::Cursor) => {
                let home = require_home_dir(home_dir)?;
                Ok(Self {
                    mcp_path: home.join(".cursor").join("mcp.json"),
                    settings_path: None,
                })
            }
            (ScopeArg::Workspace, McpProvider::Cursor) => Ok(Self {
                mcp_path: repo_root.join(".cursor").join("mcp.json"),
                settings_path: None,
            }),
            (ScopeArg::Home, McpProvider::Vscode) => {
                let home = require_home_dir(home_dir)?;
                Ok(Self {
                    mcp_path: vscode_home_user_dir(home).join("mcp.json"),
                    settings_path: None,
                })
            }
            (ScopeArg::Workspace, McpProvider::Vscode) => Ok(Self {
                mcp_path: repo_root.join(".vscode").join("mcp.json"),
                settings_path: None,
            }),
            (ScopeArg::Home, McpProvider::Windsurf) => {
                let home = require_home_dir(home_dir)?;
                Ok(Self {
                    mcp_path: home
                        .join(".codeium")
                        .join("windsurf")
                        .join("mcp_config.json"),
                    settings_path: None,
                })
            }
            (ScopeArg::Workspace, McpProvider::Windsurf) => Ok(Self {
                mcp_path: repo_root
                    .join(".codeium")
                    .join("windsurf")
                    .join("mcp_config.json"),
                settings_path: None,
            }),
        }
    }
}

/// Resolve the platform-specific VS Code "User" config directory under `home`.
///
/// VS Code stores its global `mcp.json` in this user-config folder, which
/// differs across operating systems. Centralizing the branching here keeps
/// `cfg(target_os = ...)` out of `ConfigTarget::resolve`.
pub(super) fn vscode_home_user_dir(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library")
            .join("Application Support")
            .join("Code")
            .join("User")
    }
    #[cfg(target_os = "windows")]
    {
        return home
            .join("AppData")
            .join("Roaming")
            .join("Code")
            .join("User");
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        home.join(".config").join("Code").join("User")
    }
}

fn require_home_dir(home_dir: Option<&Path>) -> Result<&Path, OrbitError> {
    home_dir.ok_or_else(|| {
        OrbitError::InvalidInput(
            "cannot resolve HOME/USERPROFILE for MCP integration files".to_string(),
        )
    })
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
    if repo_root.join(".claude").is_dir() {
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
    let antigravity_repo = repo_root.join(".agents").join("mcp_config.json").is_file()
        || repo_root.join(".agents").is_dir();
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
#[allow(clippy::too_many_arguments)]
pub(super) fn print_action_summary(
    action: McpAction<'_>,
    providers: &[McpProvider],
    repo_root: &Path,
    home_dir: Option<&Path>,
    scope: ScopeArg,
    workspace_id: Option<&str>,
) -> Result<(), OrbitError> {
    println!(
        "{}",
        format_action_summary(action, providers, repo_root, home_dir, scope, workspace_id)?
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn format_action_summary(
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
