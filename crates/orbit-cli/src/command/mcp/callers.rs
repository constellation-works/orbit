//! `orbit mcp callers` — read and seed this machine's caller authorization.
//!
//! The file is an operator artifact, so these subcommands are deliberately
//! read-mostly: `list` and `check` answer "what would this destination serve",
//! `init` transcribes machine IDs the operator already has, and `authorize`
//! renders a line for a file it does not own. Granting `operator` stays a hand
//! edit [ORB-11052], and installing an `authorized_keys` line stays an
//! operator action [ORB-11053].

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use orbit_core::OrbitError;
use orbit_core::runtime::resolve_global_root;
use orbit_mcp::{
    DefaultGrant, McpSessionAuthority, RemoteCallerIdentity, SeedCaller, SessionCapabilityPolicy,
    federated,
};
use orbit_types::identity::validate_machine_id;
use orbit_types::tool::{McpCapability, RemoteAgentInvokeMode};

use crate::command::{CommandOut, CommandOutput};

#[derive(Args)]
#[command(
    about = "Inspect and seed this machine's MCP caller authorization",
    arg_required_else_help = true,
    subcommand_required = true
)]
pub struct CallersArgs {
    #[command(subcommand)]
    pub command: CallersSubcommand,
}

#[derive(Subcommand)]
pub enum CallersSubcommand {
    /// Print the callers this machine serves and the default it applies to
    /// everyone else
    List(CallersListArgs),
    /// Print what a session from one caller would resolve to, without serving
    /// one
    Check(CallersCheckArgs),
    /// Seed a callers file from the machines this one already knows about
    ///
    /// Every seeded row is granted the agent capability. Operator is never
    /// written: which callers may dispatch work on this machine is a decision
    /// to make deliberately, one row at a time.
    Init(CallersInitArgs),
    /// Print the authorized_keys line that binds one caller to one SSH key
    ///
    /// Under that line sshd composes this machine's argv itself, so the caller
    /// identity stops being a label the caller chose and becomes something it
    /// had to hold a key to select. Tier 2 requires a dedicated Linux login
    /// account, a protected setgid Orbit launcher, a root-managed
    /// `AuthorizedKeysFile`, and per-key environments enabled.
    Authorize(CallersAuthorizeArgs),
}

#[derive(Args)]
#[command(about = "Print the callers this machine serves")]
pub struct CallersListArgs;

#[derive(Args)]
#[command(about = "Print what a session from one caller would resolve to")]
pub struct CallersCheckArgs {
    /// The calling machine's stable identity, as it would be forwarded.
    #[arg(value_name = "MACHINE_ID")]
    pub machine_id: String,
}

#[derive(Args)]
#[command(about = "Seed a callers file granting agent to known machines")]
pub struct CallersInitArgs;

#[derive(Args)]
#[command(about = "Print the authorized_keys line binding a caller to an SSH key")]
pub struct CallersAuthorizeArgs {
    /// The calling machine's stable identity, which the line will bind to the
    /// key. It is what `orbit host show` prints on the caller.
    #[arg(long, value_name = "MACHINE_ID")]
    pub machine_id: String,
    /// Path to the caller's *public* key, such as `~/.ssh/id_ed25519.pub`.
    #[arg(long, value_name = "PATH")]
    pub key: PathBuf,
    /// Absolute path to a protected copy of this Orbit binary.
    ///
    /// The copy must be root-owned, mode 2555, setgid to a group other than
    /// this account's primary group, byte-identical to the running Orbit, and
    /// configured as the dedicated account's login shell. Linux then protects
    /// the first bearer-bearing process during exec, before Orbit startup.
    #[arg(long, value_name = "PATH")]
    pub launcher: PathBuf,
}

impl CallersArgs {
    pub fn execute_without_runtime(self, root_override: Option<&Path>) -> CommandOut {
        if root_override.is_some() {
            return Err(OrbitError::InvalidInput(
                "orbit mcp callers does not accept a workspace root override; the callers file is \
                 machine-global"
                    .to_string(),
            ));
        }
        let global_root = resolve_global_root()?;
        let path = orbit_mcp::callers_path(&global_root);
        match self.command {
            CallersSubcommand::List(_) => list(&path),
            CallersSubcommand::Check(args) => check(&path, &args.machine_id),
            CallersSubcommand::Init(_) => init(&global_root, &path),
            CallersSubcommand::Authorize(args) => authorize(&global_root, &path, &args),
        }
    }
}

fn list(path: &Path) -> CommandOut {
    let file = orbit_mcp::load_callers(path)?;
    println!("callers file: {}", path.display());
    if !path.exists() {
        println!(
            "  (absent — remote-originated sessions are served agent capabilities only; run \
             `orbit mcp callers init`)"
        );
    }
    println!("default: {}", default_label(file.default));
    if file.callers.is_empty() {
        println!("callers: none");
        return Ok(CommandOutput::Silent);
    }
    println!("callers:");
    for row in &file.callers {
        let label = row
            .label
            .as_deref()
            .map(|label| format!(" ({label})"))
            .unwrap_or_default();
        println!(
            "  {machine_id}{label}: [{capabilities}]",
            machine_id = row.machine_id,
            capabilities = row.capabilities.join(", "),
        );
        if let Some(workspaces) = &row.workspaces {
            println!("    workspaces: {}", workspaces.join(", "));
        }
        if let Some(workspaces) = &row.agent_invoke_workspaces {
            println!("    agent_invoke_workspaces: {}", workspaces.join(", "));
        }
        if let Some(fingerprint) = &row.ssh_key_fingerprint {
            println!("    ssh_key_fingerprint: {fingerprint}");
        }
        println!(
            "    agent_invoke: {}",
            if row.agent_invoke {
                match row.agent_invoke_mode.unwrap_or_default() {
                    RemoteAgentInvokeMode::KeyBound => "enabled (key-bound)",
                    RemoteAgentInvokeMode::Cooperative => "enabled (cooperative)",
                }
            } else {
                "disabled"
            }
        );
    }
    Ok(CommandOutput::Silent)
}

fn check(path: &Path, machine_id: &str) -> CommandOut {
    let file = orbit_mcp::load_callers(path)?;
    // A check answers what the *grant* would be, and the grant is the same
    // under either tier — what the tier changes is whether the caller could
    // have selected this row at all, which is reported separately below.
    let identity = RemoteCallerIdentity::self_asserted(machine_id);
    let grant = file.resolve(&identity);
    println!("callers file: {}", path.display());
    println!("caller: {machine_id}");
    println!(
        "matched: {}",
        if grant.matched {
            "a row"
        } else {
            "no row — the file default applies"
        }
    );
    println!("granted: [{}]", capability_list(&grant.granted));
    println!(
        "decision: {}",
        if grant.granted.is_empty() {
            "denied"
        } else {
            "granted"
        }
    );
    match &grant.pinned_fingerprint {
        Some(fingerprint) => println!(
            "identity pin: {fingerprint}, enforced where this machine can observe the \
             authenticating key. The identity itself is key-bound only on the \
             destination-issued forced-command path; the ordinary SSH proxy remains \
             self-asserted."
        ),
        None => println!(
            "identity: self-asserted — this row is selected by a name, so any caller that \
             reaches this machine can select it. `orbit mcp callers authorize --machine-id \
             {machine_id} --key <key>.pub --launcher <protected-orbit>` prints the authorized_keys \
             line that binds it to a key."
        ),
    }
    if let Some(workspaces) = &grant.workspaces {
        println!(
            "  on workspaces: {}",
            workspaces.iter().cloned().collect::<Vec<_>>().join(", ")
        );
        println!(
            "  elsewhere on this machine: [{}]",
            capability_list(&grant.elsewhere)
        );
    }
    match (&grant.agent_invoke_workspaces, grant.agent_invoke_mode) {
        (Some(workspaces), Some(RemoteAgentInvokeMode::KeyBound)) => println!(
            "agent_invoke: configured for {} in key-bound mode; admission requires a \
             destination-issued forced-command session",
            workspaces.iter().cloned().collect::<Vec<_>>().join(", ")
        ),
        (Some(workspaces), Some(RemoteAgentInvokeMode::Cooperative)) => println!(
            "agent_invoke: configured for {} in cooperative mode; identity remains self-asserted \
             and the SSH OS-account/operator channel is the trust boundary",
            workspaces.iter().cloned().collect::<Vec<_>>().join(", ")
        ),
        _ => println!("agent_invoke: not granted"),
    }
    // Both requests, because the grant is a ceiling and the caller's argv is
    // the other half of the intersection: printing only one would read as the
    // answer to a question the caller did not ask.
    println!("a remote-originated session from this caller would hold:");
    for (label, authority) in [
        ("orbit mcp serve", McpSessionAuthority::Agent),
        ("orbit mcp serve --operator", McpSessionAuthority::Operator),
    ] {
        let policy = SessionCapabilityPolicy::from_grant(authority, file.resolve(&identity));
        // A narrowing makes "what would this session hold" a per-workspace
        // question, so answering it with one set would be a half-truth.
        match &grant.workspaces {
            None => println!(
                "  {label}: [{}]",
                capability_list(&policy.effective_for(None))
            ),
            Some(workspaces) => {
                for workspace in workspaces {
                    println!(
                        "  {label} on {workspace}: [{}]",
                        capability_list(&policy.effective_for(Some(workspace)))
                    );
                }
                println!(
                    "  {label} elsewhere: [{}]",
                    capability_list(&policy.effective_for(Some("")))
                );
            }
        }
    }
    Ok(CommandOutput::Silent)
}

fn init(global_root: &Path, path: &Path) -> CommandOut {
    let callers = known_callers(global_root)?;
    if callers.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "no other machines are known to this one, so there is nothing to seed; write '{}' by \
             hand from the caller's `machine_id`",
            path.display()
        )));
    }
    let contents = orbit_mcp::render_callers_seed(&callers);
    orbit_mcp::write_callers_seed(path, &contents)?;
    println!("wrote {}", path.display());
    for caller in &callers {
        println!("  {} granted [agent]", caller.machine_id);
    }
    println!(
        "Grant operator by editing the file; `orbit mcp callers init` never writes that \
         capability."
    );
    Ok(CommandOutput::Silent)
}

/// Render the `authorized_keys` line that pins `machine_id` to a key.
///
/// Everything an operator has to *do* goes to stderr and the artifact goes to
/// stdout alone. Orbit will not install it: the root-managed
/// `AuthorizedKeysFile` decides who may log into the machine at all, and a
/// tool that manages tasks has no business rewriting it.
fn authorize(global_root: &Path, callers_path: &Path, args: &CallersAuthorizeArgs) -> CommandOut {
    validate_machine_id(&args.machine_id).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "'{}' is not a machine identity: {error}",
            args.machine_id
        ))
    })?;
    let contents = std::fs::read_to_string(&args.key).map_err(|error| {
        OrbitError::Io(format!(
            "failed to read SSH public key '{}': {error}",
            args.key.display()
        ))
    })?;
    let key = orbit_mcp::parse_public_key(&contents)?;
    let fingerprint = key.fingerprint()?;
    let orbit_command = validate_ssh_launcher(&args.launcher)?;
    // Refuse malformed destination policy before rotating the capability and
    // invalidating an already-installed forced command.
    let file = orbit_mcp::load_callers(callers_path)?;
    let acceptance_token =
        orbit_mcp::issue_ssh_acceptance(global_root, &args.machine_id, &fingerprint)?;

    println!(
        "{}",
        key.authorized_keys_line(
            &orbit_command.to_string_lossy(),
            &args.machine_id,
            &acceptance_token
        )
    );

    eprintln!(
        "\nTier 2 requires Linux, a dedicated destination login account, and the setgid launcher \
         named in the generated command as that account's configured login shell. Keep the \
         launcher's group private to that boundary and give it no file or service privileges. The \
         Linux credential-changing login-shell exec is what hides the initial acceptance \
         environment before userspace startup; Orbit's first-line prctl is only defense in depth."
    );
    eprintln!(
        "Install the line above in a root-owned AuthorizedKeysFile that the login account cannot \
         read (for example /etc/ssh/authorized_keys/%u). In a Match User block, set \
         PermitUserEnvironment yes so sshd accepts the generated per-key acceptance environment; \
         restrict authentication to public keys from that forced-command-only file. The launcher \
         must be a root-owned, mode 2555 copy of the current Orbit binary, the login account's \
         primary group must differ from the launcher's group, and `/etc/passwd` must name this \
         launcher as that account's shell. Orbit fails closed if exec did not establish that \
         protected state. Orbit does not edit login policy."
    );
    eprintln!(
        "On every Orbit upgrade, replace the launcher with the new binary while preserving its \
         root owner, private group, and mode 2555, then re-run this command and replace the old \
         authorized_keys line. Re-authorizing rotates the acceptance value immediately; the old \
         line no longer authenticates after the record changes."
    );
    eprintln!(
        "The forced command requests operator authority, but does not grant it: the matched row \
         in the callers file remains the ceiling, so agent-only and deny rows cannot become \
         operator sessions."
    );
    eprintln!(
        "Remote `orbit.agent.invoke` remains denied unless the matched row also sets \
         `agent_invoke = true`, grants `operator`, and narrows `workspaces` to the destination \
         workspace IDs. The omitted `agent_invoke_mode` remains strict and requires this pinned \
         key. A destination owner may instead set `agent_invoke_mode = \"cooperative\"` for an \
         existing same-OS-account SSH operator channel; that records a self-asserted identity and \
         is not Tier 2. The operation grant never comes from the file default."
    );
    // The row guidance is written against what is actually in the file: a
    // template that restated `capabilities` would invite an operator to paste
    // a downgrade over a grant they had already made deliberately.
    let callers_path = callers_path.display();
    match file
        .callers
        .iter()
        .find(|row| row.machine_id == args.machine_id)
    {
        None => {
            eprintln!(
                "\nThere is no row for '{machine_id}' yet, so this caller would fall to the \
                 file default. Add one to {callers_path}:",
                machine_id = args.machine_id
            );
            eprintln!("\n  [[callers]]");
            eprintln!("  machine_id          = \"{}\"", args.machine_id);
            eprintln!("  capabilities        = [\"agent\"]");
            eprintln!("  ssh_key_fingerprint = \"{fingerprint}\"");
        }
        Some(row) if row.ssh_key_fingerprint.is_none() => {
            eprintln!(
                "\n'{machine_id}' already has a row granting [{granted}] in {callers_path}. Add \
                 the fingerprint to it — that is what turns the grant from a name into a key:",
                machine_id = args.machine_id,
                granted = row.capabilities.join(", ")
            );
            eprintln!("\n  ssh_key_fingerprint = \"{fingerprint}\"");
        }
        Some(row) if row.ssh_key_fingerprint.as_deref() != Some(fingerprint.as_str()) => eprintln!(
            "\nThe row for '{machine_id}' in {callers_path} pins a different key ({pinned}). Two \
             keys cannot both be the pinned one — replace it with {fingerprint}, or authorize the \
             key already pinned.",
            machine_id = args.machine_id,
            pinned = row.ssh_key_fingerprint.as_deref().unwrap_or("none")
        ),
        Some(_) => eprintln!(
            "\nThe row for '{machine_id}' in {callers_path} already pins this key ({fingerprint}).",
            machine_id = args.machine_id
        ),
    }
    eprintln!(
        "\nThe generated destination capability binds this forced command to the key fingerprint. \
         Re-running authorize rotates it and invalidates the previous generated line; replace the \
         root-managed entry as one rotation operation."
    );
    Ok(CommandOutput::Silent)
}

/// Validate the executable that will receive the bearer-bearing environment.
///
/// This check prevents setup from accidentally pointing at an ordinary Orbit
/// binary, where `/proc/<pid>/environ` is readable before `main`. Ownership,
/// mode, group transition, login-shell configuration, and exact binary
/// contents are all verified before a new capability rotates into use.
#[cfg(target_os = "linux")]
fn validate_ssh_launcher(path: &Path) -> Result<PathBuf, OrbitError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    if !path.is_absolute() {
        return Err(OrbitError::InvalidInput(
            "`--launcher` must be an absolute path because sshd forced commands do not use a \
             login-shell PATH"
                .to_string(),
        ));
    }
    if path
        .as_os_str()
        .to_string_lossy()
        .contains(char::is_whitespace)
    {
        return Err(OrbitError::InvalidInput(
            "`--launcher` may not contain whitespace because sshd passes the generated forced \
             command to the account's login shell as one `-c` argument"
                .to_string(),
        ));
    }
    let launcher = path.canonicalize().map_err(|error| {
        OrbitError::InvalidInput(format!(
            "SSH MCP launcher '{}' cannot be resolved: {error}",
            path.display()
        ))
    })?;
    let metadata = launcher.metadata().map_err(|error| {
        OrbitError::InvalidInput(format!(
            "SSH MCP launcher '{}' cannot be inspected: {error}",
            launcher.display()
        ))
    })?;
    let mode = metadata.permissions().mode();
    if !metadata.is_file() || metadata.uid() != 0 || mode & 0o7777 != 0o2555 {
        return Err(OrbitError::InvalidInput(format!(
            "SSH MCP launcher '{}' must be a root-owned regular setgid executable with mode 2555",
            launcher.display()
        )));
    }
    // Safety: getgid only reads the process's real group credential.
    if metadata.gid() == unsafe { libc::getgid() } {
        return Err(OrbitError::InvalidInput(format!(
            "SSH MCP launcher '{}' has the login account's real group {}; setgid exec would not \
             create the required kernel credential transition",
            launcher.display(),
            metadata.gid()
        )));
    }
    let current = std::env::current_exe().map_err(|error| {
        OrbitError::Io(format!(
            "failed to locate the running Orbit binary while validating the SSH launcher: {error}"
        ))
    })?;
    let current_bytes = std::fs::read(&current).map_err(|error| {
        OrbitError::Io(format!(
            "failed to read running Orbit binary '{}': {error}",
            current.display()
        ))
    })?;
    let launcher_bytes = std::fs::read(&launcher).map_err(|error| {
        OrbitError::Io(format!(
            "failed to read SSH MCP launcher '{}': {error}",
            launcher.display()
        ))
    })?;
    if current_bytes != launcher_bytes {
        return Err(OrbitError::InvalidInput(format!(
            "SSH MCP launcher '{}' is not a copy of the running Orbit binary; replace it before \
             rotating the authorized_keys line",
            launcher.display()
        )));
    }
    let login_shell = configured_login_shell()?;
    let login_shell = login_shell.canonicalize().map_err(|error| {
        OrbitError::InvalidInput(format!(
            "the current account's login shell '{}' cannot be resolved: {error}",
            login_shell.display()
        ))
    })?;
    if launcher != login_shell {
        return Err(OrbitError::InvalidInput(format!(
            "SSH MCP launcher '{}' is not this account's configured login shell '{}'; set the \
             dedicated account's shell to the launcher before issuing a bearer-bearing line",
            launcher.display(),
            login_shell.display()
        )));
    }
    Ok(launcher)
}

/// The current account's login shell from the system account database.
///
/// sshd always starts a forced command through that shell with `-c`. Tier 2
/// therefore requires the protected Orbit copy to be the shell itself; a
/// normal shell would receive the bearer before launching Orbit.
#[cfg(target_os = "linux")]
pub(super) fn configured_login_shell() -> Result<PathBuf, OrbitError> {
    use std::ffi::CStr;

    // Safety: getuid and sysconf have no pointer inputs or side effects.
    let uid = unsafe { libc::getuid() };
    let suggested = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
    let buffer_len = usize::try_from(suggested).unwrap_or(16 * 1024).max(1024);
    // Safety: `passwd` is a C POD whose pointer fields are populated by
    // getpwuid_r into the live buffer below before any field is read.
    let mut entry = unsafe { std::mem::zeroed::<libc::passwd>() };
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0u8; buffer_len];
    // Safety: every pointer refers to live, correctly sized writable storage;
    // getpwuid_r writes at most buffer.len() bytes and reports `result`.
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            &mut entry,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() || entry.pw_shell.is_null() {
        return Err(OrbitError::InvalidInput(format!(
            "cannot resolve the login shell for destination uid {uid}; Tier 2 requires the \
             protected launcher to be that account's shell"
        )));
    }
    // Safety: a successful getpwuid_r returned pw_shell into the still-live
    // NUL-terminated buffer.
    let shell = unsafe { CStr::from_ptr(entry.pw_shell) };
    Ok(PathBuf::from(shell.to_string_lossy().into_owned()))
}

#[cfg(not(target_os = "linux"))]
fn validate_ssh_launcher(_path: &Path) -> Result<PathBuf, OrbitError> {
    Err(OrbitError::InvalidInput(
        "SSH MCP Tier 2 launchers are supported only on Linux".to_string(),
    ))
}

/// Machines this one already names: the owners of its registered workspaces
/// and the destinations it is configured to call.
///
/// Neither list is a fleet inventory, and neither is an authorization input —
/// they are only where an operator's machine IDs are already written down, so
/// seeding does not become a transcription exercise. The accepting machine is
/// excluded: a row for itself would authorize nothing it does not already
/// resolve locally.
fn known_callers(global_root: &Path) -> Result<Vec<SeedCaller>, OrbitError> {
    // Absent host identity means this machine has no stable `machine_id`, so
    // there is nothing to exclude — and nothing that could collide either.
    let local_machine_id = match orbit_registry::inspect_host_identity(global_root)? {
        orbit_registry::HostIdentityState::Present(identity) => Some(identity.machine_id),
        _ => None,
    };
    let registry_path = orbit_registry::workspace_registry::registry_path_for(global_root);
    let registry = orbit_registry::workspace_registry::load_registry_from(&registry_path)?;
    let mut labels: BTreeMap<String, Option<String>> = BTreeMap::new();
    for workspace in &registry.workspaces {
        if let Some(owner) = &workspace.owner_machine_id {
            labels
                .entry(owner.clone())
                .or_insert_with(|| registry.owner_host_ids.get(owner).cloned());
        }
    }
    let destinations =
        federated::load_destinations(&federated::destinations_path(global_root))?.destinations;
    for destination in destinations {
        labels
            .entry(destination.machine_id.clone())
            .or_insert(Some(destination.ssh));
    }
    if let Some(local_machine_id) = &local_machine_id {
        labels.remove(local_machine_id);
    }
    Ok(labels
        .into_iter()
        .map(|(machine_id, label)| SeedCaller { machine_id, label })
        .collect())
}

fn default_label(default: DefaultGrant) -> &'static str {
    match default {
        DefaultGrant::Agent => "agent",
        DefaultGrant::Deny => "deny",
    }
}

fn capability_list(capabilities: &BTreeSet<McpCapability>) -> String {
    if capabilities.is_empty() {
        return "none".to_string();
    }
    capabilities
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
#[path = "tests/callers.rs"]
mod tests;
