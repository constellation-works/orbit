//! The plugin callback boundary: which Orbit tools a plugin backend's child
//! may reach, and the provenance its calls are audited with.

use std::cell::RefCell;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_store::Store;
use orbit_store::contracts::PluginStoreBackend;
use orbit_tools::plugin::{
    CallbackResolution, PluginCallbackIdentity, load_plugin_dir, resolve_plugin_callback_session,
};
use orbit_types::plugin::{InstalledPlugin, PluginGrant, PluginProvenance};

use crate::runtime::plugin::grants::verify_install_path;

thread_local! {
    static CALLBACK_PLUGIN_PROVENANCE: RefCell<Option<PluginProvenance>> =
        const { RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    static TEST_ACTIVITY_TOOLS: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

/// Restores a test-local activity-tool override when dropped.
///
/// The override is thread-local so tests never need to change process-global
/// environment variables merely to isolate their temporary runtime from a
/// managed executor's inherited activity allowlist.
#[cfg(test)]
pub(crate) struct TestActivityToolsGuard {
    previous: Option<Vec<String>>,
}

#[cfg(test)]
impl Drop for TestActivityToolsGuard {
    fn drop(&mut self) {
        TEST_ACTIVITY_TOOLS.with(|tools| {
            tools.replace(self.previous.take());
        });
    }
}

/// Override the effective managed-agent activity allowlist for this test
/// thread. Production dispatch always reads the inherited activity envelope.
#[cfg(test)]
pub(crate) fn override_activity_tools_for_test(
    allowed_tools: impl IntoIterator<Item = impl Into<String>>,
) -> TestActivityToolsGuard {
    let allowed_tools = allowed_tools.into_iter().map(Into::into).collect();
    let previous = TEST_ACTIVITY_TOOLS.with(|tools| tools.replace(Some(allowed_tools)));
    TestActivityToolsGuard { previous }
}

/// A process launched as a plugin backend reaches Orbit only through
/// `orbit tool run` or MCP `tools/call`, and only for tools in that plugin's
/// recorded `permissions.orbit_tools` once the host has granted `orbit_tools`.
/// Identity is the host-issued session record the backend inherits as an open
/// descriptor — not `ORBIT_PLUGIN` — and a confined descendant carrying no
/// session is refused rather than dispatched as a local caller. Anything else
/// is refused before the tool runs; a missing install, missing grant,
/// unloadable manifest, or a row whose install path is not one this host
/// installed refuses everything.
pub(super) fn enforce_plugin_callback_allowlist(
    global_root: &Path,
    plugins: &dyn PluginStoreBackend,
    name: &str,
) -> Result<(), OrbitError> {
    apply_callback_resolution(
        resolve_plugin_callback_session(global_root, || {
            legacy_callback_identity_enabled(global_root)
        })?,
        global_root,
        |plugin| plugins.get_plugin(plugin),
        name,
    )
}

pub(super) fn enforce_plugin_callback_allowlist_from_root(
    global_root: &Path,
    name: &str,
) -> Result<(), OrbitError> {
    let resolution = resolve_plugin_callback_session(global_root, || {
        legacy_callback_identity_enabled(global_root)
    })?;
    if matches!(resolution, CallbackResolution::None) {
        return Ok(());
    }
    let db =
        orbit_config::resolved_audit_db_path(&orbit_config::ConfigRoots::global_only(global_root))?;
    let store = Store::open(&db)?;
    apply_callback_resolution(
        resolution,
        global_root,
        |plugin| store.get_plugin(plugin),
        name,
    )
}

/// Refuse a plain CLI command invoked by a plugin backend [ORB-12876].
///
/// [`enforce_plugin_callback_allowlist`] gates the two entry points a backend
/// is allowed — `orbit tool run` and MCP `tools/call` — against
/// `permissions.orbit_tools`. Every *other* CLI command reads governed data
/// without ever consulting that allowlist, so a plugin granted nothing but
/// `orbit.task.list` could still run `orbit workspace list --format json` or
/// `orbit run show <id>` and read whatever the CLI exposes.
///
/// The repair is the rule the design already states
/// (`docs/design/plugins/1_scope.md` §4.2): a backend reaches Orbit only
/// through a tool call, so the CLI refuses it everything else. Scoring each
/// command against the allowlist instead would need a command-to-tool mapping
/// that does not exist and would leave every unmapped — and every newly
/// added — command open by default, which is the structural hole being
/// closed.
///
/// `invocation` names the refused command for the operator (`workspace
/// list`); it is `None` for a command that declares no audit identity.
///
/// Called from the CLI's single pre-dispatch chokepoint, before generation
/// pinning and runtime bootstrap, so a refused plugin child performs no part
/// of the command it asked for.
pub fn refuse_plugin_child_cli_command(
    global_root: &Path,
    invocation: Option<&str>,
) -> Result<(), OrbitError> {
    match resolve_plugin_callback_session(global_root, || {
        legacy_callback_identity_enabled(global_root)
    })? {
        CallbackResolution::None => Ok(()),
        CallbackResolution::Identified(identity) => Err(plugin_cli_surface_refused(
            Some(&identity.provenance.name),
            invocation,
        )),
        // A credential fault is refused on its own terms here, exactly as
        // both tool entry points refuse it, so a broken or borrowed session
        // never reads as an ordinary caller on this surface either.
        CallbackResolution::InvalidCredential(identity) => {
            Err(orbit_tools::plugin::invalid_callback_credential(
                identity.as_ref().map(|id| id.provenance.name.as_str()),
            ))
        }
        CallbackResolution::Mismatched { token, ancestry } => Err(
            orbit_tools::plugin::mismatched_callback_credential(&token, ancestry.as_deref()),
        ),
        CallbackResolution::RetiredCredential(identity) => {
            Err(orbit_tools::plugin::retired_callback_credential(
                identity.as_ref().map(|id| id.provenance.name.as_str()),
            ))
        }
        CallbackResolution::UnidentifiedPluginChild => {
            Err(orbit_tools::plugin::unidentified_plugin_child())
        }
    }
}

/// Whether this host still honours the retired plugin callback credential —
/// the environment token plus process ancestry — as well as the descriptor a
/// backend inherits [ORB-12841].
///
/// Read from the *global* `config.toml` only: a plugin backend runs under one
/// host, and a per-workspace answer would mean the same backend were
/// identified differently depending on which workspace its caller stood in.
/// Loaded on demand rather than per call, because the question only arises for
/// a caller that presented legacy evidence.
pub(crate) fn legacy_callback_identity_enabled(global_root: &Path) -> Result<bool, OrbitError> {
    let roots = orbit_config::ConfigRoots::global_only(global_root);
    Ok(orbit_config::ResolvedConfig::load(&roots)?
        .snapshot
        .plugin_legacy_callback_identity)
}

fn plugin_cli_surface_refused(plugin: Option<&str>, invocation: Option<&str>) -> OrbitError {
    let who = match plugin {
        Some(plugin) => format!("plugin '{plugin}'"),
        None => "a plugin backend".to_string(),
    };
    let what = match invocation {
        Some(invocation) => format!("run `orbit {invocation}`"),
        None => "run this command".to_string(),
    };
    OrbitError::PolicyDenied(format!(
        "{who} may not {what}; a plugin backend reaches Orbit only through `orbit tool run \
         <tool>` (or its `orbit <ns> <verb>` spelling) and MCP `tools/call` over `orbit mcp \
         serve`, and only for tools in its granted `permissions.orbit_tools` allowlist"
    ))
}

fn apply_callback_resolution(
    resolution: CallbackResolution,
    global_root: &Path,
    get_plugin: impl Fn(&str) -> Result<Option<InstalledPlugin>, OrbitError>,
    name: &str,
) -> Result<(), OrbitError> {
    match resolution {
        CallbackResolution::None => Ok(()),
        CallbackResolution::Identified(identity) => {
            let installed = get_plugin(&identity.provenance.name)?;
            stamp_callback_plugin_provenance(&identity, installed.as_ref());
            refuse_unless_recorded(global_root, installed.as_ref(), &identity, name)
        }
        CallbackResolution::InvalidCredential(identity) => {
            if let Some(identity) = identity.as_ref() {
                let installed = get_plugin(&identity.provenance.name).ok().flatten();
                stamp_callback_plugin_provenance(identity, installed.as_ref());
            }
            Err(orbit_tools::plugin::invalid_callback_credential(
                identity.as_ref().map(|id| id.provenance.name.as_str()),
            ))
        }
        CallbackResolution::Mismatched { token, ancestry } => Err(
            orbit_tools::plugin::mismatched_callback_credential(&token, ancestry.as_deref()),
        ),
        // A backend that held only the retired credential while this host has
        // stopped honouring it. Stamped like an invalid one so the refusal
        // lands on the plugin's own audit row.
        CallbackResolution::RetiredCredential(identity) => {
            if let Some(identity) = identity.as_ref() {
                let installed = get_plugin(&identity.provenance.name).ok().flatten();
                stamp_callback_plugin_provenance(identity, installed.as_ref());
            }
            Err(orbit_tools::plugin::retired_callback_credential(
                identity.as_ref().map(|id| id.provenance.name.as_str()),
            ))
        }
        // A confined backend descendant with no credential. It is not an
        // ordinary caller: `setsid` sheds ancestry, not the sandbox, and the
        // sandbox is what refused it the host-issued session directory.
        CallbackResolution::UnidentifiedPluginChild => {
            Err(orbit_tools::plugin::unidentified_plugin_child())
        }
    }
}

/// A callback may do only what *both* halves of its authority still allow.
///
/// The recorded install answers "what is this plugin granted now": it is
/// re-read on every call so revoking a grant, disabling the row, or narrowing
/// the manifest takes effect on the live session's very next callback. The
/// host-issued session answers "what was the caller that spawned this child
/// allowed to reach": it is fixed at mint time, so no later edit to the row —
/// by an operator, or by the backend itself, which can write its own install
/// tree — can hand a running child authority its caller never had
/// [ORB-12801].
///
/// Their intersection is therefore monotone downwards for the life of a
/// session: it can only ever narrow.
fn refuse_unless_recorded(
    global_root: &Path,
    installed: Option<&InstalledPlugin>,
    identity: &PluginCallbackIdentity,
    name: &str,
) -> Result<(), OrbitError> {
    let recorded = match installed {
        Some(installed) => recorded_orbit_tools(global_root, installed)?,
        None => Vec::new(),
    };
    let allowed: Vec<&String> = recorded
        .iter()
        .filter(|tool| identity.ceiling_admits(tool))
        .collect();
    if allowed.iter().any(|tool| *tool == name) {
        return Ok(());
    }
    let plugin = &identity.provenance.name;
    let mut message = format!(
        "tool '{name}' is not in plugin '{plugin}''s granted orbit_tools allowlist [{}]; the \
         manifest must request it under `permissions.orbit_tools` and the host must grant \
         `orbit_tools`",
        allowed
            .iter()
            .map(|tool| tool.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    // Separate the two refusals for the operator: a tool the plugin was never
    // granted reads differently from one it holds but this caller does not.
    if recorded.iter().any(|tool| tool == name) {
        message.push_str(&format!(
            "; the plugin does request it, but the caller that spawned this backend could reach \
             only [{}], and a live callback session's authority never widens",
            identity.effective_tools.join(", ")
        ));
    }
    Err(OrbitError::PolicyDenied(message))
}

fn stamp_callback_plugin_provenance(
    identity: &PluginCallbackIdentity,
    installed: Option<&InstalledPlugin>,
) {
    let mut provenance = identity.provenance.clone();
    if let Some(installed) = installed {
        provenance.grants = installed.grants.clone();
    }
    CALLBACK_PLUGIN_PROVENANCE.with(|cell| {
        *cell.borrow_mut() = Some(provenance);
    });
}

pub(super) fn take_callback_plugin_provenance() -> Option<PluginProvenance> {
    CALLBACK_PLUGIN_PROVENANCE.with(|cell| cell.replace(None))
}

fn recorded_orbit_tools(
    global_root: &Path,
    installed: &InstalledPlugin,
) -> Result<Vec<String>, OrbitError> {
    if !installed.enabled
        || !installed
            .grants
            .iter()
            .any(|grant| PluginGrant::parse(grant) == Some(PluginGrant::OrbitTools))
    {
        return Ok(Vec::new());
    }
    // The allowlist is read from the tree the row names, and the row is
    // writable by the very backend this gate confines. The load pass checks
    // the same thing [ORB-12785], but a backend already running can relocate
    // its row without a reload, so the path is checked again here rather than
    // reading an allowlist out of a manifest the backend wrote.
    verify_install_path(global_root, installed).map_err(OrbitError::PolicyDenied)?;
    let loaded = load_plugin_dir(Path::new(&installed.install_path)).map_err(|error| {
        OrbitError::PolicyDenied(format!(
            "plugin '{}' callback allowlist cannot be read from the recorded install: {error}",
            installed.name
        ))
    })?;
    Ok(loaded.manifest.spec.permissions.orbit_tools)
}

pub(super) fn read_proc_allowed_programs_from_env() -> Vec<String> {
    std::env::var("ORBIT_PROC_ALLOWED_PROGRAMS")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn read_activity_tools_from_env() -> Vec<String> {
    #[cfg(test)]
    if let Some(allowed_tools) = TEST_ACTIVITY_TOOLS.with(|tools| tools.borrow().clone()) {
        return allowed_tools;
    }

    if std::env::var("ORBIT_TASK_ACTOR_KIND").ok().as_deref() != Some("agent") {
        return Vec::new();
    }
    std::env::var("ORBIT_ACTIVITY_TOOLS")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}
