use orbit_engine::RuntimeHost;
#[cfg(target_os = "linux")]
use orbit_exec::{linux_bwrap_write_grant_diagnostic, prepare_linux_bwrap_write_grants};

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg(any(target_os = "macos", target_os = "linux"))]
use super::engine_support::{runtime_with_workspace_layout, seed_executor};

/// A run whose cwd is the registered checkout must not receive the policy's
/// versioned `.orbit` exceptions against that checkout's live host-clock
/// stores. The assertion goes through `resolve_executor_sandbox`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn registered_checkout_denies_live_orbit_versioned_stores() {
    if !super::isolate(
        module_path!(),
        "registered_checkout_denies_live_orbit_versioned_stores",
    ) {
        return;
    }

    use orbit_types::workflow::ExecutorSandboxKind;

    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    let repo_root = repo_root.canonicalize().expect("canonical repo");
    std::fs::create_dir_all(repo_root.join(".git")).expect("git metadata");
    let sandbox = if cfg!(target_os = "linux") {
        ExecutorSandboxKind::LinuxBwrap
    } else {
        ExecutorSandboxKind::MacosSandboxExec
    };
    for provider in ["claude", "codex"] {
        seed_executor(&runtime, provider, Some(sandbox));
        let resolved = runtime
            .resolve_executor_sandbox(provider, None, Some(&repo_root))
            .unwrap_or_else(|error| panic!("{provider} sandbox must resolve: {error}"))
            .unwrap_or_else(|| panic!("{provider} must resolve a sandbox"));
        assert_registered_host_stores_denied(&runtime, &resolved, provider);
        assert_registered_checkout_boundaries_unchanged(&runtime, &resolved, &repo_root);
    }

    #[cfg(target_os = "macos")]
    {
        let worktree = repo_root.join(".orbit/state/worktrees/orbit-jrun-registered-deny");
        std::fs::create_dir_all(worktree.join(".orbit/auto_tasks")).expect("worktree orbit");
        std::fs::create_dir_all(worktree.join(".orbit/routines")).expect("worktree routines");
        std::fs::create_dir_all(worktree.join(".orbit/resources")).expect("worktree resources");
        std::fs::write(worktree.join(".orbit/config.toml"), "versioned = true")
            .expect("worktree config");
        let resolved = runtime
            .resolve_executor_sandbox("claude", None, Some(&worktree))
            .expect("resolve worktree sandbox")
            .expect("sandbox");
        let sbpl = orbit_exec::compile_macos_sandbox_profile(&resolved.fs_profile, "claude")
            .expect("compile worktree profile");
        let orbit = worktree.join(".orbit");
        for relative in [
            "auto_tasks/child-write.yaml",
            "routines/x.yaml",
            "config.toml",
            "resources/x.yaml",
        ] {
            let path = orbit_exec::physical_with_missing_tail(&orbit.join(relative));
            assert!(
                last_compiled_file_write_allows(&sbpl, &path),
                "worktree-local `{relative}` must stay writable:\n{sbpl}"
            );
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn assert_registered_host_stores_denied(
    runtime: &orbit_core::OrbitRuntime,
    resolved: &orbit_engine::ResolvedSandbox,
    provider: &str,
) {
    let orbit = runtime
        .paths()
        .orbit_dir
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().orbit_dir.clone());
    let protected = [
        orbit.join("auto_tasks/child-write.yaml"),
        orbit.join("routines/x.yaml"),
        orbit.join("config.toml"),
        orbit.join("resources/x.yaml"),
    ];

    #[cfg(target_os = "linux")]
    {
        for path in &protected {
            let denied = linux_bwrap_write_grant_diagnostic(&resolved.fs_profile, path)
                .unwrap_or_else(|error| panic!("diagnose {}: {error}", path.display()));
            assert!(
                denied.is_some(),
                "{provider} must deny {} from the registered checkout: {denied:?}\nrules: {:?}",
                path.display(),
                resolved.fs_profile.modify
            );
        }
        for still_granted in [orbit.join("tmp/scratch"), orbit.join("tasks")] {
            assert!(
                linux_bwrap_write_grant_diagnostic(&resolved.fs_profile, &still_granted)
                    .unwrap_or_else(|error| panic!("diagnose {}: {error}", still_granted.display()))
                    .is_none(),
                "{provider} must keep {} granted: {:?}",
                still_granted.display(),
                resolved.fs_profile.modify
            );
        }
    }

    #[cfg(target_os = "macos")]
    {
        let sbpl = orbit_exec::compile_macos_sandbox_profile(&resolved.fs_profile, provider)
            .unwrap_or_else(|error| panic!("compile {provider} profile: {error}"));
        for path in &protected {
            let physical = orbit_exec::physical_with_missing_tail(path);
            assert!(
                !last_compiled_file_write_allows(&sbpl, &physical),
                "{provider} compiled profile must deny {}:\n{sbpl}",
                physical.display()
            );
        }
    }
}

/// Recovery, Git metadata, and the plugin mask are applied on the same
/// resolution that denies the registered host-clock stores.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn assert_registered_checkout_boundaries_unchanged(
    runtime: &orbit_core::OrbitRuntime,
    resolved: &orbit_engine::ResolvedSandbox,
    repo_root: &std::path::Path,
) {
    let mask = resolved
        .mask
        .as_ref()
        .expect("plugin mask stays on a sandboxed resolution");
    let targets = mask
        .targets
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    assert!(
        targets.iter().any(|path| path.ends_with("state/plugins")),
        "plugin state mask missing: {targets:?}"
    );
    assert!(
        targets
            .iter()
            .any(|path| path.ends_with("state/plugin-secrets")),
        "plugin secret mask missing: {targets:?}"
    );

    #[cfg(target_os = "linux")]
    {
        let global = runtime
            .paths()
            .global_dir
            .canonicalize()
            .unwrap_or_else(|_| runtime.paths().global_dir.clone());
        let authority = global.join("state/recovery-authority/authority.db");
        let denied = linux_bwrap_write_grant_diagnostic(&resolved.fs_profile, &authority)
            .expect("diagnose recovery authority");
        assert!(
            denied.is_some(),
            "recovery authority must stay denied: {denied:?}"
        );
        let git = repo_root
            .canonicalize()
            .expect("canonical repo")
            .join(".git");
        assert!(
            resolved
                .fs_profile
                .modify
                .iter()
                .any(|rule| rule == &format!("!{}/**", git.display())),
            "git metadata deny missing from {:?}",
            resolved.fs_profile.modify
        );
    }
    #[cfg(target_os = "macos")]
    {
        let sbpl = orbit_exec::compile_macos_sandbox_profile(&resolved.fs_profile, "claude")
            .expect("compile registered checkout profile");
        let authority = orbit_exec::physical_with_missing_tail(
            &runtime
                .paths()
                .global_dir
                .join("state/recovery-authority/authority.db"),
        );
        assert!(
            !last_compiled_file_write_allows(&sbpl, &authority),
            "recovery authority must stay denied:\n{sbpl}"
        );
        let git_config = orbit_exec::physical_with_missing_tail(&repo_root.join(".git/config"));
        assert!(
            !last_compiled_file_write_allows(&sbpl, &git_config),
            "registered Git metadata must stay denied:\n{sbpl}"
        );
    }
}

/// Last `file-write*` clause whose `subpath` or `literal` covers `path`. The
/// compiled profile denies by default, so a path with no covering clause is
/// denied.
#[cfg(target_os = "macos")]
fn last_compiled_file_write_allows(profile: &str, path: &std::path::Path) -> bool {
    last_compiled_file_write_allows_under(profile, path, std::path::Path::new("/"))
}

/// [`last_compiled_file_write_allows`] over the clauses rooted inside
/// `fixture`. Temp fixtures sit beneath the compiler's host scratch allows
/// (`/tmp`, `/private/var/folders`), which never cover a real `~/.orbit`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg(target_os = "macos")]
pub(super) fn last_compiled_file_write_allows_under(
    profile: &str,
    path: &std::path::Path,
    fixture: &std::path::Path,
) -> bool {
    last_compiled_file_write_under(profile, path, fixture) == Some(true)
}

/// Whether the last `file-write*` clause rooted inside `fixture` that covers
/// `path` allows it, or `None` when no such clause exists. A temp fixture
/// sits beneath the host scratch allows, so a `None` path is writable there
/// even though a real checkout would leave it to the default deny. A `regex`
/// clause, which policy globs such as `**/.env` compile to, counts whenever
/// it matches a `path` inside `fixture`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg(target_os = "macos")]
pub(super) fn last_compiled_file_write_under(
    profile: &str,
    path: &std::path::Path,
    fixture: &std::path::Path,
) -> Option<bool> {
    let rendered = path.display().to_string();
    let mut allowed = None;
    for line in profile.lines() {
        let (is_deny, filter) = if let Some(filter) = line.trim().strip_prefix("(deny file-write* ")
        {
            (true, filter)
        } else if let Some(filter) = line.trim().strip_prefix("(allow file-write* ") {
            (false, filter)
        } else {
            continue;
        };
        let filter = filter.trim_end_matches(')');
        let quoted = |prefix: &str| {
            filter
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_suffix('"'))
        };
        let in_fixture = if let Some(root) = quoted("(subpath \"") {
            (rendered == root || rendered.starts_with(&format!("{root}/")))
                && std::path::Path::new(root).starts_with(fixture)
        } else if let Some(entry) = quoted("(literal \"") {
            rendered == entry && std::path::Path::new(entry).starts_with(fixture)
        } else if let Some(pattern) = quoted("(regex \"") {
            sbpl_regex(pattern).is_match(&rendered) && path.starts_with(fixture)
        } else {
            continue;
        };
        if in_fixture {
            allowed = Some(!is_deny);
        }
    }
    allowed
}

/// The pattern of an SBPL `regex` filter, with its string escapes undone.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg(target_os = "macos")]
fn sbpl_regex(quoted: &str) -> regex::Regex {
    let mut pattern = String::with_capacity(quoted.len());
    let mut chars = quoted.chars();
    while let Some(c) = chars.next() {
        pattern.push(if c == '\\' {
            chars.next().expect("complete SBPL escape")
        } else {
            c
        });
    }
    regex::Regex::new(&pattern).unwrap_or_else(|error| panic!("SBPL regex `{pattern}`: {error}"))
}

/// [ORB-13458] Every positive macOS `modify` entry compiles to an SBPL write
/// allow, so provider and runtime conveniences must not hand a reviewer the
/// source checkout, its managed worktree, or primary workspace `.orbit` stores.
#[cfg(target_os = "macos")]
#[test]
fn macos_reviewer_profile_grants_no_source_or_workspace_writes() {
    if !super::isolate(
        module_path!(),
        "macos_reviewer_profile_grants_no_source_or_workspace_writes",
    ) {
        return;
    }

    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    let canonical_repo = repo_root.canonicalize().expect("canonical repo");
    let worktree = canonical_repo.join(".orbit/state/worktrees/orbit-jrun-orb-13458");
    std::fs::create_dir_all(&worktree).expect("create managed worktree");
    let inspection = tempfile::tempdir().expect("inspection checkout");
    let inspection_root = inspection
        .path()
        .canonicalize()
        .expect("canonical inspection");
    let global = runtime
        .paths()
        .global_dir
        .canonicalize()
        .expect("canonical global root")
        .display()
        .to_string();
    let protected = [
        canonical_repo.display().to_string(),
        inspection_root.display().to_string(),
    ];

    for provider in ["claude", "codex"] {
        seed_executor(
            &runtime,
            provider,
            Some(orbit_types::workflow::ExecutorSandboxKind::MacosSandboxExec),
        );
        for cwd in [&inspection_root, &worktree] {
            let sandbox = runtime
                .resolve_executor_sandbox(provider, Some("reviewer"), Some(cwd))
                .expect("resolve reviewer sandbox")
                .expect("macOS sandbox");
            let writes = sandbox
                .fs_profile
                .modify
                .iter()
                .filter(|rule| !rule.starts_with('!'))
                .collect::<Vec<_>>();
            for root in &protected {
                assert!(
                    writes.iter().all(|rule| !rule.starts_with(root.as_str())),
                    "{provider} reviewer from {} must not write under {root}: {writes:?}",
                    cwd.display()
                );
            }
            assert!(
                writes
                    .iter()
                    .any(|rule| *rule == &format!("{global}/tasks/**")),
                "{provider} reviewer keeps the global task store for nested Orbit calls: {writes:?}"
            );
            assert!(
                !writes
                    .iter()
                    .any(|rule| rule.starts_with(&format!("{global}/cache"))),
                "{provider} reviewer must not gain the implementer host cache: {writes:?}"
            );
        }
    }
}

/// [ORB-15117] A writer from a managed worktree gets the policy inside that
/// worktree as authored: its own grants, every policy deny, and nothing else
/// there or in the registered checkout. Re-allowing the bare worktree after
/// the denies let SBPL last-match-wins override them, including the `**/.env`
/// globs, which compile to `regex` clauses rather than `subpath` ones.
#[cfg(target_os = "macos")]
#[test]
fn macos_writer_from_a_managed_worktree_keeps_its_policy_inside_the_worktree() {
    if !super::isolate(
        module_path!(),
        "macos_writer_from_a_managed_worktree_keeps_its_policy_inside_the_worktree",
    ) {
        return;
    }

    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    let repo = repo_root.canonicalize().expect("canonical repo");
    let worktree = repo.join(".orbit/state/worktrees/orbit-jrun-orb-15117");
    for directory in ["a", "docs/drafts", "src"] {
        std::fs::create_dir_all(worktree.join(directory)).expect("worktree layout");
    }
    std::fs::create_dir_all(repo.join("docs")).expect("registered docs");
    let wt = |relative: &str| worktree.join(relative);
    // (profile, writable, explicitly denied, granted by no clause)
    let cases = [
        (
            "implementer",
            vec![wt("src/lib.rs")],
            vec![
                wt(".env"),
                wt(".env.local"),
                wt("a/b.env"),
                wt("a/prod.env.bak"),
            ],
            vec![repo.join("src/lib.rs")],
        ),
        (
            "docs_writer",
            vec![wt("docs/guide.md")],
            vec![wt("docs/drafts/plan.md")],
            vec![
                wt("src/lib.rs"),
                wt("README.md"),
                repo.join("docs/guide.md"),
                repo.join("src/lib.rs"),
            ],
        ),
    ];

    for provider in ["claude", "codex"] {
        seed_executor(
            &runtime,
            provider,
            Some(orbit_types::workflow::ExecutorSandboxKind::MacosSandboxExec),
        );
        for (profile, writable, denied, not_granted) in &cases {
            let sandbox = runtime
                .resolve_executor_sandbox(provider, Some(profile), Some(&worktree))
                .expect("resolve writer sandbox")
                .expect("macOS sandbox");
            let sbpl = orbit_exec::compile_macos_sandbox_profile(&sandbox.fs_profile, provider)
                .expect("compile writer profile");
            for path in writable {
                assert!(
                    last_compiled_file_write_allows_under(&sbpl, path, &repo),
                    "{provider} {profile} from a worktree must write {}:\n{sbpl}",
                    path.display()
                );
            }
            for path in denied {
                assert_eq!(
                    last_compiled_file_write_under(&sbpl, path, &repo),
                    Some(false),
                    "{provider} {profile} from a worktree must be denied {}:\n{sbpl}",
                    path.display()
                );
            }
            for path in not_granted {
                assert!(
                    !last_compiled_file_write_allows_under(&sbpl, path, &repo),
                    "{provider} {profile} from a worktree must not be granted {}:\n{sbpl}",
                    path.display()
                );
            }
            if orbit_exec::macos_sandbox_test_guard(
                "macos_writer_from_a_managed_worktree_keeps_its_policy_inside_the_worktree",
            ) {
                for path in writable {
                    assert_native_write(&sbpl, path, true);
                }
                for path in denied {
                    assert_native_write(&sbpl, path, false);
                }
            }
        }
    }
}

/// Create or append to `path` under `profile` with the native wrapper.
#[cfg(target_os = "macos")]
fn assert_native_write(profile: &str, path: &std::path::Path, allowed: bool) {
    let output = orbit_common::process::run_bounded_capped(
        std::process::Command::new("/usr/bin/sandbox-exec").args([
            "-p",
            profile,
            "/bin/sh",
            "-c",
            "printf probe >> \"$1\"",
            "write-probe",
            path.to_str().expect("fixture path"),
        ]),
        std::time::Duration::from_secs(10),
        64 * 1024,
    )
    .expect("run sandbox-exec write probe");
    assert_eq!(
        output.status.success(),
        allowed,
        "native write to {}: {output:?}",
        path.display()
    );
}

/// The recovery authority is the one durable record a resume trusts, so it
/// must sit outside every grant the leaf receives — including the run store
/// the leaf legitimately writes through `orbit.task.*` and audit tools.
#[cfg(target_os = "linux")]
#[test]
fn linux_leaf_keeps_run_store_grants_but_never_reaches_the_recovery_authority() {
    if !super::isolate(
        module_path!(),
        "linux_leaf_keeps_run_store_grants_but_never_reaches_the_recovery_authority",
    ) {
        return;
    }

    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    seed_executor(
        &runtime,
        "claude",
        Some(orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap),
    );

    let resolved = runtime
        .resolve_executor_sandbox("claude", None, Some(&repo_root))
        .expect("resolve Linux sandbox")
        .expect("descriptor");
    let global = runtime
        .paths()
        .global_dir
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().global_dir.clone());
    let authority = global.join("state/recovery-authority");

    // The database and both SQLite sidecars: a writer holding any one of them
    // could inject rows, so the deny has to cover the whole root.
    for name in ["authority.db", "authority.db-wal", "authority.db-shm"] {
        let denied =
            linux_bwrap_write_grant_diagnostic(&resolved.fs_profile, &authority.join(name))
                .expect("diagnose authority path")
                .expect("authority path must be attributably denied");
        assert!(
            denied.contains(name) && denied.contains("denyModify rule"),
            "the deny must name the path and the rule that shadows it: {denied}"
        );
    }

    // Removing the run-store grants is not the fix and must not be the effect:
    // admitted leaf tool writes keep working.
    for granted in [
        global.join("orbit.db"),
        global.join("orbit.db-wal"),
        global.join("orbit.db-shm"),
        global.join("tasks"),
        global.join("state/audit"),
    ] {
        assert!(
            linux_bwrap_write_grant_diagnostic(&resolved.fs_profile, &granted)
                .expect("diagnose granted path")
                .is_none(),
            "admitted leaf write {} must stay granted: {:?}",
            granted.display(),
            resolved.fs_profile.modify
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn resolved_sandbox_never_materializes_checkout_identity_as_a_write_anchor() {
    if !super::isolate(
        module_path!(),
        "resolved_sandbox_never_materializes_checkout_identity_as_a_write_anchor",
    ) {
        return;
    }

    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    seed_executor(
        &runtime,
        "claude",
        Some(orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap),
    );
    let worktree = runtime
        .paths()
        .orbit_dir
        .join("state/worktrees/orbit-jrun-versioned-config");
    for directory in ["auto_tasks", "routines", "resources", "state", "tasks"] {
        std::fs::create_dir_all(worktree.join(".orbit").join(directory))
            .expect("create worktree Orbit fixture");
    }
    std::fs::write(worktree.join(".orbit/config.toml"), "versioned = true")
        .expect("create versioned config fixture");

    let resolved = runtime
        .resolve_executor_sandbox("claude", None, Some(&worktree))
        .expect("resolve")
        .expect("descriptor");
    let modify = &resolved.fs_profile.modify;
    let registered_auto_tasks = runtime
        .paths()
        .orbit_dir
        .join("auto_tasks/child-write.yaml");
    assert!(
        linux_bwrap_write_grant_diagnostic(&resolved.fs_profile, &registered_auto_tasks)
            .expect("diagnose direct child write")
            .is_some(),
        "a child must not write the registered checkout's scheduler definitions"
    );
    let canonical_worktree = worktree.canonicalize().expect("canonical worktree");
    let orbit = canonical_worktree.join(".orbit");
    let deny = format!("!{}/**", orbit.display());
    let deny_pos = modify
        .iter()
        .position(|rule| rule == &deny)
        .unwrap_or_else(|| panic!("default Orbit deny missing from {modify:?}"));

    for allowed in [
        format!("{}/auto_tasks/**", orbit.display()),
        format!("{}/routines/**", orbit.display()),
        format!("{}/config.toml", orbit.display()),
        format!("{}/resources/**", orbit.display()),
    ] {
        let allow_pos = modify
            .iter()
            .position(|rule| rule == &allowed)
            .unwrap_or_else(|| panic!("versioned exception `{allowed}` missing from {modify:?}"));
        assert!(deny_pos < allow_pos, "exception must follow default deny");
    }
    let identity = orbit.join("config.yaml");
    assert!(
        linux_bwrap_write_grant_diagnostic(&resolved.fs_profile, &identity)
            .expect("diagnose runtime identity")
            .is_some(),
        "checkout-local runtime identity must remain read-only"
    );
    let prepared = prepare_linux_bwrap_write_grants(&resolved.fs_profile, &worktree)
        .expect("prepare versioned write grants");
    assert!(
        !identity.exists(),
        "an absent runtime identity must never become an empty sandbox anchor"
    );
    assert!(
        prepared.created.iter().all(|path| path != &identity),
        "runtime identity appeared in prepared anchors: {:?}",
        prepared.created
    );
    for protected in [
        format!("{}/state/**", orbit.display()),
        format!("{}/tasks/**", orbit.display()),
        format!("{}/future-store/**", orbit.display()),
    ] {
        assert!(
            !modify.iter().any(|rule| rule == &protected),
            "worktree store must not be re-allowed by policy: {protected} in {modify:?}"
        );
    }
}

/// Sandbox preparation appends runtime write roots before the recovery
/// authority deny that rejects a symlinked `<global>/state`. The later
/// rejection is not a substitute for validating the store first: without it,
/// preparation has already created directories at the redirect target by the
/// time dispatch fails.
#[cfg(target_os = "linux")]
#[test]
fn a_failed_linux_resolution_still_writes_nothing_at_a_redirect_target() {
    if !super::isolate(
        module_path!(),
        "a_failed_linux_resolution_still_writes_nothing_at_a_redirect_target",
    ) {
        return;
    }

    use std::os::unix::fs::symlink;

    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    let outside = tempfile::tempdir().expect("redirect target");
    let outside = outside.path().canonicalize().expect("canonical target");
    symlink(&outside, runtime.paths().global_dir.join("state"))
        .expect("redirect the global state store");

    seed_executor(
        &runtime,
        "claude",
        Some(orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap),
    );
    runtime
        .resolve_executor_sandbox("claude", None, Some(&repo_root))
        .expect_err("a symlinked global state root must not resolve a sandbox");

    assert!(
        !outside.join("logs").exists() && !outside.join("audit").exists(),
        "sandbox preparation must not create runtime stores at a redirect target"
    );
}
