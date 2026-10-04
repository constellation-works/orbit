use std::path::Path;
use std::path::PathBuf;

use orbit_engine::RuntimeHost;
use orbit_engine::{DispatchError, ResolvedSandbox};
use orbit_types::policy::{ResolvedFsProfile, UNRESTRICTED_FS_PROFILE};
use orbit_types::workflow::ExecutorSandboxKind;

use crate::OrbitRuntime;

#[cfg(target_os = "linux")]
use super::provider_state::append_linux_provider_state_roots;
#[cfg(target_os = "linux")]
use super::runtime_grants::append_linux_runtime_write_roots;
#[cfg(target_os = "linux")]
use super::worktree::active_worktree_subpath;
#[cfg(target_os = "macos")]
use super::worktree::append_active_worktree_root;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::worktree::recovery_checkout_root;

pub(crate) fn resolve_executor_sandbox(
    runtime: &OrbitRuntime,
    provider: &str,
    fs_profile: Option<&str>,
    subprocess_cwd: Option<&Path>,
) -> Result<Option<ResolvedSandbox>, DispatchError> {
    resolve_executor_sandbox_on(
        runtime,
        provider,
        fs_profile,
        subprocess_cwd,
        std::env::consts::OS,
    )
}

/// [`resolve_executor_sandbox`] with the host OS injected, so the refusal of a
/// sandbox kind the OS has no backend for can be exercised on any CI host.
pub(super) fn resolve_executor_sandbox_on(
    runtime: &OrbitRuntime,
    provider: &str,
    fs_profile: Option<&str>,
    subprocess_cwd: Option<&Path>,
    host_os: &str,
) -> Result<Option<ResolvedSandbox>, DispatchError> {
    let executor = runtime.get_executor_def(provider).map_err(|err| {
        DispatchError::CliInvocationFailed(format!(
            "load executor `{provider}` for sandbox resolution: {err}"
        ))
    })?;
    let Some(executor) = executor else {
        return Ok(None);
    };
    let Some(kind) = executor.sandbox else {
        return Ok(None);
    };
    // A declared sandbox is a host precondition, never a runtime fallback:
    // refuse before any provider argv is built or child spawned.
    if !kind.is_available_on(host_os) {
        return Err(DispatchError::CliInvocationPermanent(
            sandbox_unavailable_message(provider, kind, host_os),
        ));
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let recovery_checkout = subprocess_cwd.and_then(|cwd| recovery_checkout_root(runtime, cwd));
    match kind {
        // Carry explicit off through preparation so the runner can suppress
        // provider-inner sandboxing and audit the choice without probing an OS
        // wrapper or resolving filesystem grants that will not be enforced.
        ExecutorSandboxKind::Off => Ok(Some(ResolvedSandbox {
            kind,
            fs_profile: ResolvedFsProfile {
                name: UNRESTRICTED_FS_PROFILE.to_string(),
                read: Vec::new(),
                modify: Vec::new(),
            },
            allow_fallback: false,
            managed_worktree: false,
            runtime_write_authority: Vec::new(),
            mask: None,
        })),
        ExecutorSandboxKind::MacosSandboxExec => {
            #[cfg(not(target_os = "macos"))]
            {
                Err(DispatchError::CliInvocationPermanent(
                    sandbox_unavailable_message(provider, kind, std::env::consts::OS),
                ))
            }
            #[cfg(target_os = "macos")]
            {
                // Read-only reviewer activities may run from an invocation-owned
                // inspection checkout, so their read grants must follow that
                // checkout. Recovery also needs its own checkout-relative
                // grants and denies. Other implementer profiles stay anchored
                // at the registered workspace, with the active worktree
                // re-allowed separately below.
                let profile_root = if fs_profile == Some("reviewer") || recovery_checkout.is_some()
                {
                    subprocess_cwd
                } else {
                    None
                };
                let mut resolved = resolve_fs_profile_absolute(runtime, fs_profile, profile_root)
                    .map_err(|err| {
                    DispatchError::CliInvocationFailed(format!(
                        "resolve fsProfile for sandbox: {err}"
                    ))
                })?;
                // Same boundary as linux-bwrap: an activity profile with an
                // empty modify surface stays a non-writer of the source tree
                // and the primary workspace. Every positive entry below compiles
                // to an SBPL write allow, so Codex side roots, workspace
                // `.orbit` stores, and the active managed worktree are gated on
                // the profile itself. Provider state directories come from the
                // SBPL compiler and global Orbit runtime stores stay available.
                let grants_workspace_modify =
                    resolved.modify.iter().any(|rule| !rule.starts_with('!'));
                if grants_workspace_modify {
                    append_codex_side_write_roots(runtime, provider, &mut resolved)?;
                }
                append_orbit_child_runtime_write_roots(
                    runtime,
                    grants_workspace_modify,
                    &mut resolved,
                );
                if grants_workspace_modify {
                    append_active_worktree_root(runtime, subprocess_cwd, &mut resolved);
                }
                deny_registered_auto_task_definition_writes(runtime, &mut resolved);
                append_recovery_authority_deny(runtime, &mut resolved)?;
                deny_recovery_checkout_orbit(recovery_checkout.as_deref(), &mut resolved);
                Ok(Some(ResolvedSandbox {
                    kind,
                    fs_profile: resolved,
                    allow_fallback: executor.allow_fallback,
                    managed_worktree: recovery_checkout.is_some(),
                    runtime_write_authority: Vec::new(),
                    mask: Some(agent_plugin_mask(runtime)?),
                }))
            }
        }
        ExecutorSandboxKind::LinuxBwrap => {
            #[cfg(not(target_os = "linux"))]
            {
                Err(DispatchError::CliInvocationPermanent(
                    sandbox_unavailable_message(provider, kind, std::env::consts::OS),
                ))
            }
            #[cfg(target_os = "linux")]
            {
                let mut resolved = resolve_fs_profile_absolute(runtime, fs_profile, subprocess_cwd)
                    .map_err(|err| {
                        DispatchError::CliInvocationFailed(format!(
                            "resolve fsProfile for linux-bwrap: {err}"
                        ))
                    })?;
                // Runtime and provider conveniences must not turn an activity
                // profile with an empty modify surface into a workspace writer.
                // Besides violating that profile, workspace re-allows make a
                // direct Bubblewrap invocation unable to enforce global
                // non-subtree denies such as `**/.env` for paths created after
                // spawn. Provider state and global Orbit runtime roots remain
                // available below; neither overlaps workspace-relative denies.
                let grants_workspace_modify =
                    resolved.modify.iter().any(|rule| !rule.starts_with('!'));
                if grants_workspace_modify {
                    append_codex_side_write_roots(runtime, provider, &mut resolved)?;
                }
                let mut runtime_write_authority = Vec::new();
                append_linux_runtime_write_roots(
                    runtime,
                    subprocess_cwd,
                    grants_workspace_modify,
                    &mut resolved,
                    &mut runtime_write_authority,
                )?;
                append_linux_provider_state_roots(provider, &mut resolved)?;
                append_recovery_authority_deny(runtime, &mut resolved)?;
                // Host Git state is never a provider convenience grant. Append
                // these last so even a side root inside metadata stays denied.
                crate::runtime::git_sandbox::append_linux_git_denies(
                    &runtime.paths().repo_root,
                    &mut resolved,
                )
                .map_err(|error| DispatchError::CliInvocationPermanent(error.to_string()))?;
                if let Some(cwd) = subprocess_cwd {
                    crate::runtime::git_sandbox::append_linux_git_denies(cwd, &mut resolved)
                        .map_err(|error| {
                            DispatchError::CliInvocationPermanent(error.to_string())
                        })?;
                }
                deny_recovery_checkout_orbit(recovery_checkout.as_deref(), &mut resolved);
                // Recovery is host-prepared too. Its policy includes future
                // filename denies that need the same post-run guard as a task
                // worktree, even after the .orbit mount anchor exists.
                let managed_worktree = recovery_checkout.is_some()
                    || subprocess_cwd
                        .and_then(|cwd| active_worktree_subpath(runtime, cwd))
                        .is_some();
                Ok(Some(ResolvedSandbox {
                    kind,
                    fs_profile: resolved,
                    allow_fallback: executor.allow_fallback,
                    managed_worktree,
                    runtime_write_authority,
                    mask: Some(agent_plugin_mask(runtime)?),
                }))
            }
        }
    }
}

/// Recovery has no writable Orbit stores. Preserve only its existing scratch
/// exception, with subsequent deny rules, after all convenience grants.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn deny_recovery_checkout_orbit(checkout: Option<&Path>, resolved: &mut ResolvedFsProfile) {
    if let Some(checkout) = checkout {
        let scratch = format!("{}/.orbit/tmp/**", checkout.display());
        let scratch_rules = resolved
            .modify
            .iter()
            .position(|rule| rule == &scratch)
            .map(|index| {
                resolved.modify[index..]
                    .iter()
                    .filter(|rule| *rule == &scratch || rule.starts_with('!'))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        resolved
            .modify
            .push(format!("!{}/.orbit/**", checkout.display()));
        resolved.modify.extend(scratch_rules);
    }
}

/// Refusal for an executor whose declared sandbox has no backend on `host_os`.
///
/// Names both remedies: the platform's native backend where one exists, and
/// otherwise WSL2 (the supported Windows runtime). The explicit, audited
/// `spec.sandbox: off` opt-out applies everywhere.
fn sandbox_unavailable_message(provider: &str, kind: ExecutorSandboxKind, host_os: &str) -> String {
    let native = [
        ExecutorSandboxKind::MacosSandboxExec,
        ExecutorSandboxKind::LinuxBwrap,
    ]
    .into_iter()
    .find(|candidate| candidate.target_os() == Some(host_os));
    let remedy = match native {
        Some(native) => format!(
            "set `spec.sandbox: {native}` on the executor, or `spec.sandbox: off` to run it explicitly without an OS sandbox"
        ),
        None => format!(
            "Orbit has no sandbox backend for `{host_os}`; on Windows run Orbit inside WSL2, or set `spec.sandbox: off` on the executor to run it explicitly without an OS sandbox"
        ),
    };
    format!(
        "executor `{provider}` declares sandbox `{kind}` but current platform is `{host_os}`; {remedy}"
    )
}

/// Hide plugin state and the plugin secret store from the sandboxed process
/// (design `docs/design/plugins/2_agent_call_broker.md` §6).
///
/// Every sandboxed launch gets the mask, whether or not its run's broker then
/// binds: a broker that fails costs the run its plugin calls, never the mask.
/// A host that cannot lay the mask refuses the launch rather than start the
/// agent with the trees readable.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn agent_plugin_mask(
    runtime: &OrbitRuntime,
) -> Result<orbit_engine::SandboxMask, DispatchError> {
    let prepared =
        crate::runtime::plugin::sandbox_mask::prepare_plugin_mask(&runtime.global_root())
            .map_err(|error| DispatchError::CliInvocationPermanent(error.to_string()))?;
    Ok(orbit_engine::SandboxMask {
        sentinel: prepared.sentinel,
        targets: prepared.trees,
    })
}

/// Resolve the activity's fsProfile against the active policy, then expand
/// every workspace-relative `read` / `modify` rule to an absolute path under
/// the workspace root. The kernel's `subpath` predicate is meaningless for
/// relative paths, so this is the layer that turns Orbit's policy into a
/// payload `sandbox-exec` can enforce.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn resolve_fs_profile_absolute(
    runtime: &OrbitRuntime,
    fs_profile: Option<&str>,
    workspace_override: Option<&Path>,
) -> Result<ResolvedFsProfile, orbit_common::OrbitError> {
    let profile_name = fs_profile.unwrap_or(UNRESTRICTED_FS_PROFILE);
    let resolved = runtime
        .policy_engine()
        .def()
        .effective_profile(profile_name)?;
    let workspace_root = workspace_override
        .unwrap_or(&runtime.paths().repo_root)
        .canonicalize()
        .unwrap_or_else(|_| {
            workspace_override.map_or_else(
                || runtime.paths().repo_root.clone(),
                std::path::Path::to_path_buf,
            )
        });
    let workspace_str = workspace_root.display().to_string();

    Ok(ResolvedFsProfile {
        name: resolved.name,
        read: resolved
            .read
            .into_iter()
            .map(|rule| absolutize_rule(&workspace_str, &rule))
            .collect(),
        modify: resolved
            .modify
            .into_iter()
            .map(|rule| absolutize_rule(&workspace_str, &rule))
            .collect(),
    })
}

fn append_codex_side_write_roots(
    runtime: &OrbitRuntime,
    provider: &str,
    resolved: &mut ResolvedFsProfile,
) -> Result<(), DispatchError> {
    // Codex is the only `backend: cli` provider that ships its own writable
    // root surface (`--add-dir` fed from `writable_dirs_json`). Claude and
    // Gemini have no analogous CLI flag — their startup-time writes are
    // confined to their state directories, which `compile_macos_sandbox_profile`
    // already grants via the per-provider state-dir allowances. If a future
    // provider gains a side-root surface, add a sibling appender. See
    // T20260428-14.
    if provider != "codex" {
        return Ok(());
    }

    let config = RuntimeHost::agent_provider_config(runtime);
    let Some(raw_dirs) = config.get("writable_dirs_json") else {
        return Ok(());
    };
    let writable_dirs: Vec<String> = serde_json::from_str(raw_dirs).map_err(|err| {
        DispatchError::CliInvocationFailed(format!(
            "parse codex writable_dirs_json for sandbox: {err}"
        ))
    })?;
    if writable_dirs.is_empty() {
        return Ok(());
    }

    let workspace_root = runtime
        .paths()
        .repo_root
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().repo_root.clone());
    let workspace_str = workspace_root.display().to_string();
    for dir in writable_dirs {
        let Some(root) = absolutize_side_write_root(&workspace_str, &dir) else {
            continue;
        };
        // Append even when the root already appears earlier: SBPL is
        // last-match-wins, and these host-owned roots must land after
        // policy-derived denies such as `.orbit/**`.
        resolved.modify.push(root);
    }
    Ok(())
}

/// Allow the nested Orbit processes launched by provider CLIs to initialize
/// only the runtime stores they need while staying inside the outer sandbox.
///
/// Gemini, Antigravity, and Claude do not have a codex-style `--add-dir` side
/// channel, but their MCP/tool calls still execute `orbit ...` as a
/// sandbox-inherited child.
/// Those child processes initialize global logs/audit/databases/tasks plus the
/// workspace stores exposed by activity tool allowlists.
///
/// Inventory boundary: this list follows currently activity-exposed Orbit write
/// tools. Registered-but-not-exposed stores such as ADRs and graph write roots
/// stay denied until the corresponding tools are added to those activity
/// allowlists. Keep the grants path-shaped instead of re-allowing the whole
/// home directory or workspace `.orbit` tree.
///
/// Global runtime stores are granted when they resolve inside the global root.
/// The host cache and workspace stores also require `grants_workspace_modify`,
/// so a read-only activity profile never becomes a primary-workspace writer.
/// Every store must stay inside its runtime root after symlink resolution.
#[cfg(any(target_os = "macos", all(target_os = "linux", test)))]
pub(super) fn append_orbit_child_runtime_write_roots(
    runtime: &OrbitRuntime,
    grants_workspace_modify: bool,
    resolved: &mut ResolvedFsProfile,
) {
    let global_root = runtime
        .paths()
        .global_dir
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().global_dir.clone());
    let workspace_orbit = runtime
        .paths()
        .orbit_dir
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().orbit_dir.clone());
    for (relative, suffix) in [
        ("state/logs", "/**"),
        ("state/audit", "/**"),
        ("orbit.db", "*"),
        ("tasks", "/**"),
    ] {
        append_contained_runtime_modify_root(&global_root, relative, suffix, resolved);
    }

    if !grants_workspace_modify {
        return;
    }

    // Language-neutral host cache seam shared across worktrees. Not an
    // activity-tool store and not a shared Cargo target directory.
    // Implementer-only, as on Linux. [ORB-11259]
    append_contained_runtime_modify_root(&global_root, "cache", "/**", resolved);
    for (relative, suffix) in [
        ("tasks", "/**"),
        ("frictions", "/**"),
        ("state/audit", "/**"),
        ("state/logs", "/**"),
        ("state/semantic.db", "*"),
    ] {
        append_contained_runtime_modify_root(&workspace_orbit, relative, suffix, resolved);
    }
}

/// Match the SBPL compiler's physical path identity before granting a store,
/// including missing descendants of existing symlinked ancestors. Drop an
/// unresolved link as well: it could acquire an outside target before the
/// compiler resolves the rule. Safe stores need not exist yet.
#[cfg(any(target_os = "macos", all(target_os = "linux", test)))]
fn append_contained_runtime_modify_root(
    root: &Path,
    relative: &str,
    suffix: &str,
    resolved: &mut ResolvedFsProfile,
) {
    let physical = orbit_exec::physical_with_missing_tail(&root.join(relative));
    let mut ancestor = physical.as_path();
    let contained = physical.starts_with(root)
        && loop {
            match std::fs::symlink_metadata(ancestor) {
                Ok(metadata) => break !metadata.is_symlink(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let Some(parent) = ancestor.parent() else {
                        break false;
                    };
                    ancestor = parent;
                }
                Err(_) => break false,
            }
        };
    if !contained {
        tracing::warn!(
            runtime_root = %root.display(),
            store = relative,
            "skipping sandbox grant for a runtime store whose physical containment cannot be established"
        );
        return;
    }
    append_unique_modify_root(resolved, format!("{}{suffix}", physical.display()));
}

/// Keep registered scheduler definitions behind the host-brokered auto-task
/// tools. The default policy exception is for host-side writes; nested provider
/// children must not inherit it as direct filesystem authority.
#[cfg(any(target_os = "macos", all(target_os = "linux", test)))]
pub(super) fn deny_registered_auto_task_definition_writes(
    runtime: &OrbitRuntime,
    resolved: &mut ResolvedFsProfile,
) {
    let workspace_orbit = runtime
        .paths()
        .orbit_dir
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().orbit_dir.clone());
    let auto_tasks = workspace_orbit.join("auto_tasks").display().to_string();
    let auto_tasks_descendants = format!("{auto_tasks}/");

    resolved.modify.retain(|rule| {
        if rule.starts_with('!') {
            return true;
        }
        rule != &auto_tasks && !rule.starts_with(&auto_tasks_descendants)
    });
    resolved.modify.push(format!("!{auto_tasks}/**"));
}

/// Deny the host-only recovery authority store, after every convenience grant.
///
/// No grant above names this root, so the rule is a tripwire that keeps a
/// future broadening of the `<global>` grants from reopening it. Both sandbox
/// kinds get it: the boundary is a property of the store, not of one OS.
fn append_recovery_authority_deny(
    runtime: &OrbitRuntime,
    resolved: &mut ResolvedFsProfile,
) -> Result<(), DispatchError> {
    crate::runtime::recovery_authority::append_recovery_authority_denies(
        &runtime.paths().global_dir,
        resolved,
    )
    .map_err(|error| DispatchError::CliInvocationPermanent(error.to_string()))
}

pub(super) fn append_unique_modify_root(resolved: &mut ResolvedFsProfile, root: String) {
    if !resolved.modify.iter().any(|entry| entry == &root) {
        resolved.modify.push(root);
    }
}

fn absolutize_side_write_root(workspace_root: &str, path: &str) -> Option<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }
    let absolute = if PathBuf::from(trimmed).is_absolute() {
        PathBuf::from(trimmed)
    } else {
        let trimmed = trimmed.trim_start_matches("./");
        if trimmed.is_empty() || trimmed == "." {
            PathBuf::from(workspace_root)
        } else {
            PathBuf::from(workspace_root).join(trimmed)
        }
    };
    let normalized = absolute.canonicalize().unwrap_or(absolute);
    Some(normalized.display().to_string())
}

fn absolutize_rule(workspace_root: &str, rule: &str) -> String {
    let (negated, body) = rule
        .strip_prefix('!')
        .map(|rest| (true, rest))
        .unwrap_or((false, rule));
    let trimmed = body.trim_start_matches("./");
    let absolute = if PathBuf::from(trimmed).is_absolute() {
        trimmed.to_string()
    } else if trimmed.is_empty() || trimmed == "." {
        workspace_root.to_string()
    } else {
        format!("{}/{}", workspace_root.trim_end_matches('/'), trimmed)
    };
    if negated {
        format!("!{absolute}")
    } else {
        absolute
    }
}
