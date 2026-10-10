//! Where each supported MCP client keeps its server registrations, and
//! whether Orbit's server is registered there for a workspace.
//!
//! `orbit mcp init/remove` writes these files and `orbit doctor`'s
//! `mcp-registration` row reads them, from the CLI and the dashboard alike, so
//! the path table lives here once. Nothing in this module writes a file.

use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;

/// The server name Orbit registers under in every client's config.
pub const ORBIT_MCP_SERVER_ID: &str = "orbit";

/// A supported MCP client integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum McpClient {
    Claude,
    Codex,
    Gemini,
    Antigravity,
    Grok,
    Cursor,
    Vscode,
    Windsurf,
}

impl McpClient {
    /// Every supported client, in the order reports list them.
    pub const ALL: [Self; 8] = [
        Self::Claude,
        Self::Codex,
        Self::Gemini,
        Self::Antigravity,
        Self::Grok,
        Self::Cursor,
        Self::Vscode,
        Self::Windsurf,
    ];

    /// The client's CLI name, as `--client` spells it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Antigravity => "antigravity",
            Self::Grok => "grok",
            Self::Cursor => "cursor",
            Self::Vscode => "vscode",
            Self::Windsurf => "windsurf",
        }
    }
}

/// Whether a registration lives in the user's home or in the repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpClientScope {
    Home,
    Workspace,
}

/// Config files for one client and scope.
///
/// `mcp_path` is the active registry, `legacy_mcp_path` is a migration source,
/// and `settings_path` is Claude's optional permissions file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpClientConfigPaths {
    pub mcp_path: PathBuf,
    pub legacy_mcp_path: Option<PathBuf>,
    pub settings_path: Option<PathBuf>,
}

impl McpClientConfigPaths {
    fn only(mcp_path: PathBuf) -> Self {
        Self {
            mcp_path,
            legacy_mcp_path: None,
            settings_path: None,
        }
    }
}

/// Resolve the config files `client` reads at `scope`. Home scope needs a
/// home directory; workspace scope resolves under `repo_root`.
pub fn client_config_paths(
    scope: McpClientScope,
    client: McpClient,
    repo_root: &Path,
    home_dir: Option<&Path>,
) -> Result<McpClientConfigPaths, OrbitError> {
    use McpClientScope::{Home, Workspace};

    Ok(match (scope, client) {
        (Home, McpClient::Claude) => {
            let home = require_home_dir(home_dir)?;
            McpClientConfigPaths {
                mcp_path: home.join(".claude.json"),
                legacy_mcp_path: Some(home.join(".claude").join(".mcp.json")),
                settings_path: Some(home.join(".claude").join("settings.json")),
            }
        }
        (Workspace, McpClient::Claude) => McpClientConfigPaths {
            mcp_path: repo_root.join(".mcp.json"),
            legacy_mcp_path: Some(repo_root.join(".claude.json")),
            settings_path: Some(repo_root.join(".claude").join("settings.json")),
        },
        (Home, McpClient::Codex) => McpClientConfigPaths::only(
            require_home_dir(home_dir)?
                .join(".codex")
                .join("config.toml"),
        ),
        (Workspace, McpClient::Codex) => {
            McpClientConfigPaths::only(repo_root.join(".codex").join("config.toml"))
        }
        (Home, McpClient::Gemini) => McpClientConfigPaths::only(
            require_home_dir(home_dir)?
                .join(".gemini")
                .join("settings.json"),
        ),
        (Workspace, McpClient::Gemini) => {
            McpClientConfigPaths::only(repo_root.join(".gemini").join("settings.json"))
        }
        (Home, McpClient::Antigravity) => McpClientConfigPaths::only(
            require_home_dir(home_dir)?
                .join(".gemini")
                .join("config")
                .join("mcp_config.json"),
        ),
        (Workspace, McpClient::Antigravity) => {
            McpClientConfigPaths::only(repo_root.join(".agents").join("mcp_config.json"))
        }
        (Home, McpClient::Grok) => {
            let home = require_home_dir(home_dir)?;
            if grok_reads_shared_location(scope, Some(home)) {
                McpClientConfigPaths {
                    mcp_path: home.join(".claude.json"),
                    legacy_mcp_path: Some(home.join(".grok").join("config.toml")),
                    settings_path: None,
                }
            } else {
                McpClientConfigPaths::only(home.join(".grok").join("config.toml"))
            }
        }
        (Workspace, McpClient::Grok) => {
            let shared = grok_reads_shared_location(scope, home_dir);
            McpClientConfigPaths {
                mcp_path: if shared {
                    repo_root.join(".mcp.json")
                } else {
                    repo_root.join(".grok").join("config.toml")
                },
                legacy_mcp_path: shared.then(|| repo_root.join(".grok").join("config.toml")),
                settings_path: None,
            }
        }
        (Home, McpClient::Cursor) => {
            McpClientConfigPaths::only(require_home_dir(home_dir)?.join(".cursor").join("mcp.json"))
        }
        (Workspace, McpClient::Cursor) => {
            McpClientConfigPaths::only(repo_root.join(".cursor").join("mcp.json"))
        }
        (Home, McpClient::Vscode) => McpClientConfigPaths::only(
            vscode_home_user_dir(require_home_dir(home_dir)?).join("mcp.json"),
        ),
        (Workspace, McpClient::Vscode) => {
            McpClientConfigPaths::only(repo_root.join(".vscode").join("mcp.json"))
        }
        (Home, McpClient::Windsurf) => McpClientConfigPaths::only(
            require_home_dir(home_dir)?
                .join(".codeium")
                .join("windsurf")
                .join("mcp_config.json"),
        ),
        (Workspace, McpClient::Windsurf) => McpClientConfigPaths::only(
            repo_root
                .join(".codeium")
                .join("windsurf")
                .join("mcp_config.json"),
        ),
    })
}

/// Grok stops reading project `.mcp.json` after Claude import, and its
/// `~/.claude.json` reader can be disabled separately. Use Grok's native
/// config when the shared reader is unavailable or cannot be checked.
fn grok_reads_shared_location(scope: McpClientScope, home_dir: Option<&Path>) -> bool {
    let Some(home) = home_dir else {
        return false;
    };
    let path = home.join(".grok").join("config.toml");
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
        Err(_) => return false,
    };
    let Ok(config) = raw.parse::<toml::Value>() else {
        return false;
    };
    if config
        .get("claude_compat")
        .and_then(|compat| compat.get("imported"))
        .and_then(toml::Value::as_bool)
        == Some(true)
    {
        return false;
    }
    if scope == McpClientScope::Home {
        if config
            .get("compat")
            .and_then(|compat| compat.get("claude"))
            .and_then(|claude| claude.get("mcps"))
            .and_then(toml::Value::as_bool)
            == Some(false)
        {
            return false;
        }
        if std::env::var("GROK_CLAUDE_MCPS_ENABLED")
            .ok()
            .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "0" | "false"))
        {
            return false;
        }
    }
    true
}

/// Resolve the platform-specific VS Code "User" config directory under `home`.
///
/// VS Code stores its global `mcp.json` in this user-config folder, which
/// differs across operating systems. Centralizing the branching here keeps
/// `cfg(target_os = ...)` out of [`client_config_paths`].
pub fn vscode_home_user_dir(home: &Path) -> PathBuf {
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

/// The error for a home-scoped target when no home directory resolves.
pub fn require_home_dir(home_dir: Option<&Path>) -> Result<&Path, OrbitError> {
    home_dir.ok_or_else(|| {
        OrbitError::InvalidInput(
            "cannot resolve HOME/USERPROFILE for MCP integration files".to_string(),
        )
    })
}

/// Return client registrations for Orbit's MCP server in this workspace.
/// Reuse the same paths that `mcp init` writes, including
/// user-level registrations, without starting a client or touching its files.
pub fn registered_clients_for_workspace(
    repo_root: &Path,
    workspace_id: Option<&str>,
    home_dir: Option<&Path>,
) -> Vec<String> {
    let mut found = Vec::new();
    for scope in [McpClientScope::Workspace, McpClientScope::Home] {
        for client in McpClient::ALL {
            let Ok(paths) = client_config_paths(scope, client, repo_root, home_dir) else {
                continue;
            };
            if [Some(paths.mcp_path), paths.legacy_mcp_path]
                .into_iter()
                .flatten()
                .any(|path| registration_matches(&path, client, workspace_id, repo_root))
            {
                found.push(format!(
                    "{} ({})",
                    client.label(),
                    if scope == McpClientScope::Home {
                        "home"
                    } else {
                        "workspace"
                    }
                ));
            }
        }
    }
    found
}

fn registration_matches(
    path: &Path,
    client: McpClient,
    workspace_id: Option<&str>,
    repo_root: &Path,
) -> bool {
    let Ok(contents) = fs::read_to_string(path) else {
        return false;
    };
    let entry = if client == McpClient::Codex
        || (client == McpClient::Grok && path.extension().is_some_and(|ext| ext == "toml"))
    {
        let Ok(doc) = contents.parse::<toml::Value>() else {
            return false;
        };
        doc.get("mcp_servers")
            .and_then(|servers| servers.get(ORBIT_MCP_SERVER_ID))
            .and_then(|server| serde_json::to_value(server).ok())
    } else {
        let Ok(doc) = serde_json::from_str::<serde_json::Value>(&contents) else {
            return false;
        };
        let key = if client == McpClient::Vscode {
            "servers"
        } else {
            "mcpServers"
        };
        doc.get(key)
            .and_then(|servers| servers.get(ORBIT_MCP_SERVER_ID))
            .cloned()
    };
    let Some(entry) = entry else {
        return false;
    };
    if entry.get("enabled").and_then(serde_json::Value::as_bool) == Some(false) {
        return false;
    }
    // A client may launch Orbit through a wrapper or connect to a remote
    // server. This row checks registration, not whether the launch succeeds.
    let has_launch = ["command", "url"].into_iter().any(|key| {
        entry
            .get(key)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    });
    if !has_launch {
        return false;
    }
    let bound = entry
        .get("args")
        .and_then(serde_json::Value::as_array)
        .and_then(|args| {
            args.windows(2).find_map(|pair| {
                (pair[0].as_str() == Some("--workspace"))
                    .then(|| pair[1].as_str())
                    .flatten()
            })
        });
    bound.is_none_or(|bound| workspace_id == Some(bound) || bound == repo_root.to_string_lossy())
}
