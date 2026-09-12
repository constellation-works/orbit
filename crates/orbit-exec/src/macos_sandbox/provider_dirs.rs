use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::sbpl_filter::{push_regex_escaped, push_regex_escaped_str, sbpl_escape};

pub(super) fn provider_state_dirs(
    home: Option<&OsStr>,
    codex_home: Option<&OsStr>,
    claude_config_dir: Option<&OsStr>,
    grok_home: Option<&OsStr>,
) -> Vec<PathBuf> {
    let mut dirs = Vec::with_capacity(4);
    if let Some(dir) = codex_state_dir(home, codex_home) {
        dirs.push(dir);
    }
    if let Some(dir) = claude_state_dir(home, claude_config_dir) {
        dirs.push(dir);
    }
    if let Some(dir) = gemini_state_dir(home) {
        dirs.push(dir);
    }
    if let Some(dir) = grok_state_dir(home, grok_home) {
        dirs.push(dir);
    }
    dirs
}

fn codex_state_dir(home: Option<&OsStr>, codex_home: Option<&OsStr>) -> Option<PathBuf> {
    non_empty_env_path(codex_home)
        .or_else(|| non_empty_env_path(home).map(|path| path.join(".codex")))
}

/// Claude Code documents `CLAUDE_CONFIG_DIR` as the override; otherwise the
/// CLI writes settings, sessions, projects, file-history, and todos under
/// `$HOME/.claude`.
fn claude_state_dir(home: Option<&OsStr>, claude_config_dir: Option<&OsStr>) -> Option<PathBuf> {
    non_empty_env_path(claude_config_dir)
        .or_else(|| non_empty_env_path(home).map(|path| path.join(".claude")))
}

/// Process-env wrapper around [`claude_state_dir`]. Returns the writable
/// state directory Claude Code uses at runtime — `$CLAUDE_CONFIG_DIR` if
/// set, otherwise `$HOME/.claude`. Returns `None` only when both env vars
/// are unset or empty. Callers in `backend: cli` use this to land
/// auxiliary CLI outputs (e.g. `--debug-file`) at a sandbox-allowed path
/// instead of the workspace, where `denyModify: .orbit/**` would block
/// startup-time writes.
pub fn claude_state_dir_from_env() -> Option<PathBuf> {
    let claude_config_dir = std::env::var_os("CLAUDE_CONFIG_DIR");
    let home = std::env::var_os("HOME");
    claude_state_dir(home.as_deref(), claude_config_dir.as_deref())
}

/// Gemini CLI and Antigravity CLI both write under `$HOME/.gemini`
/// (Antigravity uses `$HOME/.gemini/antigravity-cli/`). Neither documents a
/// stable env override. If a future CLI release surfaces one, plumb it
/// through `SandboxCompileEnv` here. [ORB-11299]
fn gemini_state_dir(home: Option<&OsStr>) -> Option<PathBuf> {
    non_empty_env_path(home).map(|path| path.join(".gemini"))
}

/// Grok Build documents `GROK_HOME` as the override for its config/state
/// directory; otherwise it writes under `$HOME/.grok`.
// pub(crate) widened for sibling-layout tests in macos_sandbox/tests/provider_dirs.rs (ORB-00241)
pub(crate) fn grok_state_dir(home: Option<&OsStr>, grok_home: Option<&OsStr>) -> Option<PathBuf> {
    non_empty_env_path(grok_home)
        .or_else(|| non_empty_env_path(home).map(|path| path.join(".grok")))
}

/// Process-env wrapper around [`grok_state_dir`]. Returns the writable state
/// directory Grok Build uses at runtime — `$GROK_HOME` if set, otherwise
/// `$HOME/.grok`. Returns `None` only when both env vars are unset or empty.
pub fn grok_state_dir_from_env() -> Option<PathBuf> {
    let home = std::env::var_os("HOME");
    let grok_home = std::env::var_os("GROK_HOME");
    grok_state_dir(home.as_deref(), grok_home.as_deref())
}

/// Writable directories an **active** Copilot executor needs, in the order
/// they are granted. [ORB-10946]
///
/// Unlike [`provider_state_dirs`], this is gated on the active provider by its
/// caller. Copilot's cache entry is a package-extraction directory, not
/// per-tool configuration, so granting it to every provider would hand each
/// one a writable path it has no reason to touch.
///
/// 1. `$COPILOT_HOME`, else `$HOME/.copilot` — the CLI's documented
///    configuration and state directory (credentials from `copilot /login`,
///    session history, `mcp-config.json`, and `logs/`).
/// 2. `$XDG_CACHE_HOME/copilot`, else `$HOME/.cache/copilot` — the standalone
///    CLI ships as a launcher that extracts its bundled package here on first
///    run. Without this the CLI aborts before it ever reads Orbit's envelope,
///    with `ENOENT: no such file or directory, mkdir '<...>/.cache/copilot'`.
pub(crate) fn copilot_state_dirs(
    home: Option<&OsStr>,
    copilot_home: Option<&OsStr>,
    xdg_cache_home: Option<&OsStr>,
) -> Vec<PathBuf> {
    let mut dirs = Vec::with_capacity(2);
    if let Some(dir) = copilot_state_dir(home, copilot_home) {
        dirs.push(dir);
    }
    if let Some(dir) = copilot_cache_dir(home, xdg_cache_home) {
        dirs.push(dir);
    }
    dirs
}

/// Copilot CLI documents `COPILOT_HOME` as the override for the directory
/// where configuration and state files are stored; otherwise it uses
/// `$HOME/.copilot`.
pub(crate) fn copilot_state_dir(
    home: Option<&OsStr>,
    copilot_home: Option<&OsStr>,
) -> Option<PathBuf> {
    non_empty_env_path(copilot_home)
        .or_else(|| non_empty_env_path(home).map(|path| path.join(".copilot")))
}

/// The Copilot launcher's bundled-package extraction directory. Honors
/// `XDG_CACHE_HOME` before falling back to `$HOME/.cache`.
pub(crate) fn copilot_cache_dir(
    home: Option<&OsStr>,
    xdg_cache_home: Option<&OsStr>,
) -> Option<PathBuf> {
    non_empty_env_path(xdg_cache_home)
        .map(|path| path.join("copilot"))
        .or_else(|| non_empty_env_path(home).map(|path| path.join(".cache").join("copilot")))
}

/// Cursor CLI stores configuration, permissions, and session state under
/// `$HOME/.cursor`. On macOS the default logged-in credential lives in the
/// login keychain (`cursor-access-token` / `cursor-refresh-token`); the file
/// store at `$HOME/.cursor/auth.json` is used only when
/// `AGENT_CLI_CREDENTIAL_STORE=file` was set at login time. `CURSOR_API_KEY`
/// is the environment opt-in that skips both stores. The caller gates this
/// directory on an active Cursor executor so other providers do not receive
/// Cursor-specific write access. [ORB-10945] [ORB-12261]
pub(crate) fn cursor_state_dir(home: Option<&OsStr>) -> Option<PathBuf> {
    non_empty_env_path(home).map(|path| path.join(".cursor"))
}

/// Pi CLI stores login credentials, settings, trust decisions, packages, and
/// session state under its agent directory: `$PI_CODING_AGENT_DIR` when set,
/// otherwise `$HOME/.pi` (whose `agent/` subdirectory is the documented
/// default). The caller gates this directory on an active Pi executor so other
/// providers do not receive Pi-specific write access. [ORB-11296]
pub(crate) fn pi_state_dir(
    home: Option<&OsStr>,
    pi_coding_agent_dir: Option<&OsStr>,
) -> Option<PathBuf> {
    non_empty_env_path(pi_coding_agent_dir)
        .or_else(|| non_empty_env_path(home).map(|path| path.join(".pi")))
}

/// Writable directories an **active** OpenCode executor needs. [ORB-11295]
///
/// OpenCode resolves every root through `xdg-basedir` and creates `data`,
/// `config`, and `state` at startup — before it ever reads Orbit's envelope —
/// so all of them must be writable or the CLI aborts during initialization.
/// `cache` is included because the same startup path takes a lock under it.
///
/// 1. `$XDG_DATA_HOME/opencode`, else `$HOME/.local/share/opencode` —
///    `auth.json` from `opencode auth login`, session/message stores, `log/`,
///    and `repos/`.
/// 2. `$OPENCODE_CONFIG_DIR`, else `$XDG_CONFIG_HOME/opencode`, else
///    `$HOME/.config/opencode` — `opencode.json` plus the `agents/`,
///    `commands/`, `plugins/`, `skills/`, `tools/`, and `themes/` trees.
/// 3. `$XDG_STATE_HOME/opencode`, else `$HOME/.local/state/opencode`.
/// 4. `$XDG_CACHE_HOME/opencode`, else `$HOME/.cache/opencode`.
///
/// Like Copilot's, Cursor's, and Pi's entries, this is gated on the active
/// provider by its caller so an unrelated lane is not handed OpenCode's
/// credential store.
pub(crate) fn opencode_state_dirs(env: OpencodeDirEnv<'_>) -> Vec<PathBuf> {
    let OpencodeDirEnv {
        home,
        xdg_data_home,
        xdg_config_home,
        xdg_state_home,
        xdg_cache_home,
        opencode_config_dir,
    } = env;
    let mut dirs = Vec::with_capacity(4);
    let mut push = |dir: Option<PathBuf>| {
        if let Some(dir) = dir {
            dirs.push(dir);
        }
    };
    push(xdg_scoped_dir(home, xdg_data_home, &[".local", "share"]));
    push(
        non_empty_env_path(opencode_config_dir)
            .or_else(|| xdg_scoped_dir(home, xdg_config_home, &[".config"])),
    );
    push(xdg_scoped_dir(home, xdg_state_home, &[".local", "state"]));
    push(xdg_scoped_dir(home, xdg_cache_home, &[".cache"]));
    dirs
}

/// Env inputs that locate OpenCode's XDG roots.
#[derive(Default, Clone, Copy)]
pub(crate) struct OpencodeDirEnv<'a> {
    pub(crate) home: Option<&'a OsStr>,
    pub(crate) xdg_data_home: Option<&'a OsStr>,
    pub(crate) xdg_config_home: Option<&'a OsStr>,
    pub(crate) xdg_state_home: Option<&'a OsStr>,
    pub(crate) xdg_cache_home: Option<&'a OsStr>,
    pub(crate) opencode_config_dir: Option<&'a OsStr>,
}

/// Resolve one `<xdg base>/opencode` root: the explicit XDG variable when set,
/// otherwise its specified default relative to `$HOME`.
fn xdg_scoped_dir(
    home: Option<&OsStr>,
    xdg_base: Option<&OsStr>,
    home_relative_default: &[&str],
) -> Option<PathBuf> {
    let base = non_empty_env_path(xdg_base).or_else(|| {
        non_empty_env_path(home).map(|home| {
            home_relative_default
                .iter()
                .fold(home, |path, segment| path.join(segment))
        })
    })?;
    Some(base.join("opencode"))
}

pub(super) fn non_empty_env_path(value: Option<&OsStr>) -> Option<PathBuf> {
    let value = value?;
    if value.to_string_lossy().is_empty() {
        return None;
    }
    Some(PathBuf::from(value))
}

/// Claude Code persists its main settings to `$HOME/.claude.json`, a sibling
/// *file* (with `.lock` and atomic-write `.tmp.<pid>.<ms_ts>` siblings) of
/// the `$HOME/.claude/` directory. SBPL `subpath` does not match these
/// siblings, so the per-provider state-dir clause emitted by
/// `provider_state_dirs` is not enough — Claude under sandbox would hang
/// waiting on its own lockfile.
///
/// Skip when `CLAUDE_CONFIG_DIR` is set: with the override, Claude writes
/// `<override>/.claude.json` (and the lock/tmp siblings) inside the override
/// directory, already covered by the existing subpath clause.
pub(super) fn emit_claude_home_json_allows(
    home: Option<&OsStr>,
    claude_config_dir: Option<&OsStr>,
    out: &mut String,
) {
    if non_empty_env_path(claude_config_dir).is_some() {
        return;
    }
    let Some(home) = non_empty_env_path(home) else {
        return;
    };
    let home_str = home.display().to_string();
    out.push_str(&format!(
        "(allow file-write* (literal \"{}/.claude.json\"))\n",
        sbpl_escape(&home_str)
    ));
    out.push_str(&format!(
        "(allow file-write* (literal \"{}/.claude.json.lock\"))\n",
        sbpl_escape(&home_str)
    ));
    let mut tmp_regex = String::from("^");
    for c in home_str.chars() {
        push_regex_escaped(&mut tmp_regex, c);
    }
    tmp_regex.push_str("/\\.claude\\.json\\.tmp\\.[0-9]+\\.[0-9]+$");
    out.push_str(&format!(
        "(allow file-write* (regex \"{}\"))\n",
        sbpl_escape(&tmp_regex)
    ));
}

/// Grok keeps its JSON state and companion lock/tmp files under `GROK_HOME`
/// (default `$HOME/.grok`). The state-dir `subpath` allow already covers
/// these, but emitting explicit rules mirrors Claude's lockfile treatment and
/// keeps the startup-critical files visible in compiled profiles.
pub(super) fn emit_grok_state_file_allows(
    home: Option<&OsStr>,
    grok_home: Option<&OsStr>,
    out: &mut String,
) {
    let Some(state_dir) = grok_state_dir(home, grok_home) else {
        return;
    };

    for file_name in ["auth.json", "mcp_credentials.json", "models_cache.json"] {
        emit_grok_json_file_allow(&state_dir, file_name, out);
    }

    let state_dir_str = state_dir.display().to_string();
    let mut lock_regex = String::from("^");
    push_regex_escaped_str(&mut lock_regex, &state_dir_str);
    lock_regex.push_str("/mcp_auth_[^/]+\\.lock$");
    out.push_str(&format!(
        "(allow file-write* (regex \"{}\"))\n",
        sbpl_escape(&lock_regex)
    ));
}

fn emit_grok_json_file_allow(state_dir: &Path, file_name: &str, out: &mut String) {
    let path = state_dir.join(file_name);
    let path_str = path.display().to_string();
    out.push_str(&format!(
        "(allow file-write* (literal \"{}\"))\n",
        sbpl_escape(&path_str)
    ));
    out.push_str(&format!(
        "(allow file-write* (literal \"{}.lock\"))\n",
        sbpl_escape(&path_str)
    ));

    let mut tmp_regex = String::from("^");
    push_regex_escaped_str(&mut tmp_regex, &path_str);
    tmp_regex.push_str("\\.tmp(?:\\.[0-9]+)*$");
    out.push_str(&format!(
        "(allow file-write* (regex \"{}\"))\n",
        sbpl_escape(&tmp_regex)
    ));
}
