use std::path::Path;

use crate::command::mcp::ORBIT_MCP_SERVER_ID;

pub(in crate::command::mcp::setup) const ORBIT_FEDERATED_MCP_SERVER_ID: &str = "orbit-federated";

/// The launch identity a generated client config is written for.
///
/// Local and federated servers are separate variants because the federated mux
/// cannot carry local operator authority or a local workspace binding. The
/// registration path chooses the variant explicitly, so bare `orbit mcp init`
/// cannot silently convert an existing v1 integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::command::mcp::setup) enum ServerLaunch<'a> {
    /// The v1 server, optionally operator-authorized and workspace-bound.
    ///
    /// `orbit_root` is the catalog a client must open when that catalog is
    /// not the host-global default. It is absent for a default-root launch.
    /// Federated entries never carry it: a local root is not applied to SSH
    /// destinations.
    Local {
        operator: bool,
        workspace: Option<&'a str>,
        orbit_root: Option<&'a Path>,
    },
    /// The session-unbound mux over operator-configured destinations.
    Federated,
}

impl Default for ServerLaunch<'_> {
    fn default() -> Self {
        Self::Local {
            operator: false,
            workspace: None,
            orbit_root: None,
        }
    }
}

impl<'a> ServerLaunch<'a> {
    pub(in crate::command::mcp::setup) const fn local(
        operator: bool,
        workspace: Option<&'a str>,
    ) -> Self {
        Self::Local {
            operator,
            workspace,
            orbit_root: None,
        }
    }

    /// Attach the catalog root selected for this registration.
    ///
    /// Federated launches ignore it. Passing `None` restores the historical
    /// default-root argv. The returned launch lives only as long as `orbit_root`.
    pub(in crate::command::mcp::setup) fn with_orbit_root<'b>(
        self,
        orbit_root: Option<&'b Path>,
    ) -> ServerLaunch<'b>
    where
        'a: 'b,
    {
        match self {
            Self::Local {
                operator,
                workspace,
                ..
            } => ServerLaunch::Local {
                operator,
                workspace,
                orbit_root,
            },
            Self::Federated => ServerLaunch::Federated,
        }
    }

    const fn orbit_root(self) -> Option<&'a Path> {
        match self {
            Self::Local { orbit_root, .. } => orbit_root,
            Self::Federated => None,
        }
    }
}

pub(super) fn server_id(launch: ServerLaunch<'_>) -> &'static str {
    match launch {
        ServerLaunch::Local { .. } => ORBIT_MCP_SERVER_ID,
        ServerLaunch::Federated => ORBIT_FEDERATED_MCP_SERVER_ID,
    }
}

/// The argv a generated client config launches Orbit's MCP server with.
///
/// A non-default catalog is a global `--root` before the `mcp` subcommand,
/// so the client process reaches that registry without inheriting
/// `ORBIT_ROOT`. Federated argv is unchanged.
pub(super) fn server_args(launch: ServerLaunch<'_>) -> Vec<String> {
    let mut args = Vec::new();
    match launch {
        ServerLaunch::Local {
            operator,
            workspace,
            orbit_root,
        } => {
            if let Some(root) = orbit_root.filter(|root| !root.as_os_str().is_empty()) {
                args.push("--root".to_string());
                args.push(root.to_string_lossy().into_owned());
            }
            args.push("mcp".to_string());
            args.push("serve".to_string());
            if operator {
                args.push("--operator".to_string());
            }
            if let Some(workspace) = workspace {
                args.push("--workspace".to_string());
                args.push(workspace.to_string());
            }
        }
        ServerLaunch::Federated => {
            args.push("mcp".to_string());
            args.push("serve".to_string());
            args.push("--mode".to_string());
            args.push("federated".to_string());
        }
    }
    args
}

/// `--root` / catalog path to insert into an existing launch that does not
/// already select a root.
///
/// An existing `--root` or a non-empty `ORBIT_ROOT` binding belongs to that
/// launch. Reconciliation supplies a missing root and does not replace one
/// the operator already chose.
pub(super) fn missing_root_args<'b>(
    args: impl IntoIterator<Item = &'b str>,
    env_root: Option<&str>,
    launch: ServerLaunch<'_>,
) -> Option<[String; 2]> {
    let root = launch
        .orbit_root()
        .filter(|root| !root.as_os_str().is_empty())?;
    if env_root.is_some_and(|value| !value.trim().is_empty()) {
        return None;
    }
    let args = args.into_iter().collect::<Vec<_>>();
    if args.windows(2).any(|pair| pair[0] == "--root")
        || args.iter().any(|arg| arg.starts_with("--root="))
    {
        return None;
    }
    Some(["--root".to_string(), root.to_string_lossy().into_owned()])
}

/// Insert the selected catalog root into a preserved JSON server entry.
///
/// Returns whether `entry` changed. Entries whose `args` are not all strings
/// are left untouched.
pub(super) fn supply_json_root(entry: &mut serde_json::Value, launch: ServerLaunch<'_>) -> bool {
    let env_root = entry
        .get("env")
        .and_then(|env| env.get("ORBIT_ROOT"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let Some(args) = entry
        .get_mut("args")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return false;
    };
    if args.iter().any(|value| !value.is_string()) {
        return false;
    }
    let prefix = {
        let current = args
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>();
        missing_root_args(current, env_root.as_deref(), launch)
    };
    let Some([flag, root]) = prefix else {
        return false;
    };
    args.insert(0, serde_json::Value::String(root));
    args.insert(0, serde_json::Value::String(flag));
    true
}

/// Reconciliation must retain the operator's existing launch, including its
/// binding and any other options. Removing the entry first is the explicit
/// way to replace that authority with a new agent-only registration.
pub(super) fn preserve_operator<'a>(
    launch: ServerLaunch<'_>,
    args: impl IntoIterator<Item = &'a str>,
) -> bool {
    let args = args.into_iter().collect::<Vec<_>>();
    matches!(launch, ServerLaunch::Local { .. })
        && args.windows(2).any(|pair| pair == ["mcp", "serve"])
        && args.contains(&"--operator")
}

pub(super) fn preserve_json_operator(launch: ServerLaunch<'_>, entry: &serde_json::Value) -> bool {
    entry
        .get("args")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|args| {
            preserve_operator(launch, args.iter().filter_map(serde_json::Value::as_str))
        })
}
