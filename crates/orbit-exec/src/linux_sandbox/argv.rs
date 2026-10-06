use super::*;

/// Compile a deterministic Bubblewrap argv. Broad writable roots are emitted
/// before every deny mount. A positive exact/subtree rule that is strictly
/// nested under an earlier deny is emitted in rule order as an explicit narrow
/// re-allow; positive ancestors and equal roots cannot override a deny.
///
/// A narrow re-allow whose anchor is absent cannot be mounted. It is reported
/// on [`LinuxBwrapPlan::dropped_grants`] rather than dropped, so the caller can
/// attribute the resulting denial to a path and a rule instead of leaving the
/// sandboxed process to interpret an EROFS.
///
/// A write-capable profile also binds Cargo's shared download caches writable
/// before any policy mount — see [`append_cargo_download_cache_mounts`] for why
/// the read-only bind of `/` cannot stand for them.
///
/// Every plan also hides the host account's well-known credential locations
/// (`~/.ssh`, `~/.aws`, `~/.config/gh`, cargo publish tokens) behind an empty
/// stand-in, after every other mount — see [`super::credentials`].
///
/// The profile's globs are compiled once and every non-subtree rule is
/// expanded up front from one walk per search root; the mount loops and the
/// post-run guard read that expansion rather than walking again.
pub fn compile_linux_bwrap_argv(
    profile: &ResolvedFsProfile,
    program: &str,
    args: &[String],
    cwd: Option<&Path>,
    managed_worktree: bool,
) -> Result<LinuxBwrapPlan, OrbitError> {
    compile_plan(profile, program, args, cwd, managed_worktree, None)
}

fn compile_plan(
    profile: &ResolvedFsProfile,
    program: &str,
    args: &[String],
    cwd: Option<&Path>,
    managed_worktree: bool,
    mask: Option<&LinuxBwrapMask>,
) -> Result<LinuxBwrapPlan, OrbitError> {
    compile_plan_with_credentials(
        profile,
        program,
        args,
        cwd,
        managed_worktree,
        mask,
        &host_credential_denies(),
        host_mounts,
    )
}

/// [`compile_plan`] with the credential locations to mask passed explicitly,
/// so the placement can be exercised against a fixture home instead of the
/// host account's.
#[allow(clippy::too_many_arguments)]
pub(super) fn compile_plan_with_credentials(
    profile: &ResolvedFsProfile,
    program: &str,
    args: &[String],
    cwd: Option<&Path>,
    managed_worktree: bool,
    mask: Option<&LinuxBwrapMask>,
    credentials: &[CredentialReadDeny],
    mounts: impl FnOnce() -> Result<Vec<MountEntry>, OrbitError>,
) -> Result<LinuxBwrapPlan, OrbitError> {
    let compiled = CompiledModifyRules::compile(profile)?;
    let expanded = expand_each_rule(
        profile
            .modify
            .iter()
            .map(|rule| rule.strip_prefix('!').unwrap_or(rule))
            .filter(|body| !is_exact_or_subtree(body)),
    )?;
    let mut out = base_namespace_args();
    let mut dropped_grants = Vec::new();
    // Replace mutable host pseudo-filesystems and scratch before applying
    // policy mounts. A writable worktree nested below `/tmp` is then re-bound
    // narrowly without exposing the rest of the host scratch tree.
    out.extend([
        "--dev".to_string(),
        "/dev".to_string(),
        "--proc".to_string(),
        "/proc".to_string(),
        "--tmpfs".to_string(),
        "/tmp".to_string(),
    ]);
    let cwd = cwd
        .map(|cwd| canonical_existing(cwd, "sandbox cwd"))
        .transpose()?;
    if let Some(cwd) = &cwd {
        append_git_metadata_mounts(&mut out, cwd)?;
    }
    let writable_roots = positive_mount_roots(profile, &expanded)?;
    let mut anchors = BTreeSet::new();
    let mut scratch_anchors = BTreeSet::new();
    for denied in profile
        .modify
        .iter()
        .filter_map(|rule| rule.strip_prefix('!'))
    {
        for path in mount_paths_for_rule(denied, false, &expanded)? {
            for ancestor in path.ancestors().skip(1) {
                if writable_roots.iter().any(|root| ancestor.starts_with(root)) {
                    anchors.insert(ancestor.to_path_buf());
                } else if ancestor.starts_with("/tmp") && ancestor != Path::new("/tmp") {
                    scratch_anchors.insert(ancestor.to_path_buf());
                }
            }
        }
    }
    // Bubblewrap creates missing mount parents on the private /tmp tmpfs.
    // Those scaffold entries are writable even without a positive rule, so a
    // leaf could rename a denied path's ancestor or plant redirects there.
    // Restore them read-only, parent first, before granting any child writes.
    // Keep /tmp itself private and writable for ordinary process scratch.
    for anchor in scratch_anchors {
        push_mount(&mut out, "--ro-bind", &anchor);
    }
    // Before any policy mount, so a policy deny that covers one of these paths
    // is still emitted afterwards and still wins. A profile with no positive
    // modify rule gains no writable bind at all, cargo caches included.
    if profile_grants_write(profile) {
        append_cargo_download_cache_mounts(&mut out, cargo_home_dir().as_deref());
    }
    for (index, rule) in profile.modify.iter().enumerate() {
        if rule.starts_with('!') || is_narrow_reallow(&profile.modify[..index], rule) {
            continue;
        }
        for path in mount_paths_for_rule(rule, true, &expanded)? {
            push_mount(&mut out, "--bind", &path);
        }
    }
    // Bind every writable ancestor entry of an existing deny before applying
    // restrictions. Linux permits renaming an ancestor of a mount; making each
    // such entry a mountpoint prevents moving it aside to replace the path.
    for anchor in anchors {
        let rendered = anchor.display().to_string();
        let already_mounted = out
            .windows(3)
            .any(|args| args[0] == "--bind" && args[1] == rendered && args[2] == rendered);
        if !already_mounted {
            push_mount(&mut out, "--bind", &anchor);
        }
    }

    for (index, rule) in profile.modify.iter().enumerate() {
        if let Some(denied) = rule.strip_prefix('!') {
            let paths = mount_paths_for_rule(denied, false, &expanded)?;
            // A missing exact/subtree root cannot receive a read-only mount.
            // A direct invocation has no post-run guard to catch its creation.
            if !managed_worktree
                && overlaps_writable_root(denied, &writable_roots)
                && (!is_exact_or_subtree(denied) || paths.is_empty())
            {
                let reason = if paths.is_empty() && is_exact_or_subtree(denied) {
                    "absent"
                } else {
                    "non-subtree"
                };
                return Err(OrbitError::PolicyDenied(format!(
                    "linux-bwrap cannot enforce {reason} denyModify `{denied}` for a direct invocation; use a managed worktree"
                )));
            }
            for path in paths {
                push_mount(&mut out, "--ro-bind", &path);
            }
        } else if is_narrow_reallow(&profile.modify[..index], rule) {
            let Some(anchor) = exact_or_subtree_root(rule) else {
                continue;
            };
            if !compiled.grants_write(&anchor) {
                continue;
            }
            let paths = mount_paths_for_rule(rule, false, &expanded)?;
            if paths.is_empty() {
                dropped_grants.push(UnsatisfiedWriteGrant {
                    rule: rule.clone(),
                    anchor,
                    reason: "no path on disk matches the rule, so Bubblewrap has nothing to bind and the grant stays under the surrounding read-only mount".to_string(),
                });
                continue;
            }
            for path in paths {
                push_mount(&mut out, "--bind", &path);
            }
        }
    }

    if let Some(cwd) = &cwd
        && managed_worktree
        && cwd_is_writable_root(cwd, &writable_roots)
    {
        append_stable_toolchain_mounts(&mut out, cwd)?;
    }
    // After every policy mount and alias bind, so none of them can expose a
    // masked location again.
    append_credential_masks(&mut out, credentials, mounts)?;
    if let Some(mask) = mask {
        append_mask_mounts(&mut out, mask)?;
    }
    if let Some(cwd) = cwd {
        // Keep the provider agent on the real worktree path. rustc cache-key
        // cwd normalization belongs in scripts/rustc-compiler-cache.sh, which
        // chdirs onto LINUX_STABLE_WORKSPACE_MOUNT only for compiler invocations.
        out.push("--chdir".to_string());
        out.push(cwd.display().to_string());
    }
    out.push("--".to_string());
    out.push(program.to_string());
    out.extend(args.iter().cloned());

    // Only a managed worktree carries a post-run guard: a direct invocation
    // refused every overlapping deny that could not be mounted at spawn.
    let post_run_guard = if managed_worktree {
        LinuxBwrapPostRunGuard::from_expansion(profile, &expanded)
    } else {
        None
    };

    Ok(LinuxBwrapPlan {
        wrapper: bwrap_program_for_audit().to_string(),
        args: out,
        dropped_grants,
        mount_sources: Vec::new(),
        mount_evidence: Vec::new(),
        post_run_guard,
    })
}

/// Compile a plan whose selected writable mount sources are descriptor-backed.
///
/// The authority descriptors name the validated objects even if their host
/// paths are subsequently replaced. Bubblewrap receives only inherited
/// `--bind-fd` sources for those grants; a missing matching bind fails
/// closed because the runtime authority would otherwise be silently unused.
///
/// `mask`, when given, is mounted after every other mount of the plan; a
/// masked directory the child could also reach through another path refuses
/// the plan (see [`LinuxBwrapMask`]).
pub fn compile_linux_bwrap_argv_with_authority(
    profile: &ResolvedFsProfile,
    program: &str,
    args: &[String],
    cwd: Option<&Path>,
    managed_worktree: bool,
    authority: Vec<LinuxBwrapMountAuthority>,
    mask: Option<&LinuxBwrapMask>,
) -> Result<LinuxBwrapPlan, OrbitError> {
    let mut plan = compile_plan(profile, program, args, cwd, managed_worktree, mask)?;
    for grant in authority {
        let source = prepare_mount_source(grant.source)?;
        #[cfg(unix)]
        let source_fd = source.as_raw_fd();
        #[cfg(unix)]
        let metadata = source.metadata().map_err(|error| {
            OrbitError::Execution(format!(
                "inspect Linux runtime grant descriptor for `{}`: {error}",
                grant.destination.display()
            ))
        })?;
        let rendered = grant.destination.display().to_string();
        let mut replaced = false;
        for index in 0..plan.args.len().saturating_sub(2) {
            if plan.args[index] == "--bind"
                && plan.args[index + 1] == rendered
                && plan.args[index + 2] == rendered
            {
                plan.args[index] = "--bind-fd".to_string();
                // The retained File reserves this exact descriptor while
                // Command builds its pipes. Rendering that descriptor here
                // keeps argv and child inheritance on one source of truth.
                #[cfg(unix)]
                {
                    plan.args[index + 1] = source_fd.to_string();
                }
                replaced = true;
            }
        }
        if !replaced {
            return Err(OrbitError::PolicyDenied(format!(
                "validated Linux runtime grant `{}` had no writable mount in the final sandbox plan",
                grant.destination.display()
            )));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            plan.mount_evidence.push(LinuxBwrapMountEvidence {
                destination: grant.destination,
                source_fd,
                device: metadata.dev(),
                inode: metadata.ino(),
            });
        }
        plan.mount_sources.push(source);
    }
    Ok(plan)
}

pub(super) fn base_namespace_args() -> Vec<String> {
    [
        "--die-with-parent",
        "--new-session",
        "--unshare-all",
        "--share-net",
        "--ro-bind",
        "/",
        "/",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}
