use super::*;

/// The filesystem shape a granted write anchor must have for Bubblewrap to
/// bind it. Derived from the granting rule, never from a table of known paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteAnchorKind {
    File,
    Directory,
}

/// One narrow write exception the effective profile grants, together with the
/// mount anchor Bubblewrap needs on disk in order to honor it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriteGrant {
    /// The `modify` rule, exactly as the effective profile spells it.
    pub rule: String,
    /// The path that must exist for the rule to become a mount.
    pub anchor: PathBuf,
    pub kind: WriteAnchorKind,
}

/// A grant that policy expresses but the mount plan cannot honor. Carries the
/// path *and* the rule so a denial is attributable without reading argv.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnsatisfiedWriteGrant {
    pub rule: String,
    pub anchor: PathBuf,
    pub reason: String,
}

impl UnsatisfiedWriteGrant {
    pub fn describe(&self) -> String {
        format!(
            "write grant `{}` (rule `{}`) was not applied: {}",
            self.anchor.display(),
            self.rule,
            self.reason
        )
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PreparedWriteGrants {
    /// Anchors materialized by this call, in profile order.
    pub created: Vec<PathBuf>,
    /// Grants left unmountable because their anchor is absent and lies outside
    /// the trusted preparation root, so creating it is not this layer's call.
    pub unsatisfied: Vec<UnsatisfiedWriteGrant>,
}

/// Every narrow write re-allow in the effective profile — the positive
/// exact/subtree rules nested under an earlier deny — paired with the anchor
/// each one needs. Broad writable roots are excluded: they are required to
/// exist by [`compile_linux_bwrap_argv`] and are not exceptions to a deny.
///
/// This is the whole grant set. It is read off the profile that will compile
/// the argv, so it cannot drift from what the sandbox actually enforces.
pub fn linux_bwrap_write_grants(
    profile: &ResolvedFsProfile,
) -> Result<Vec<WriteGrant>, OrbitError> {
    let compiled = CompiledModifyRules::compile(profile)?;
    let mut grants = Vec::new();
    for (index, rule) in profile.modify.iter().enumerate() {
        if rule.starts_with('!') || !is_narrow_reallow(&profile.modify[..index], rule) {
            continue;
        }
        let Some(anchor) = exact_or_subtree_root(rule) else {
            continue;
        };
        // A later deny that covers the anchor shadows this re-allow under the
        // profile's last-match-wins contract. Do not materialize a path the
        // final policy denies. A narrower deny below a subtree does not match
        // the subtree root, so the remaining writable portion is preserved.
        if !compiled.grants_write(&anchor) {
            continue;
        }
        grants.push(WriteGrant {
            rule: rule.clone(),
            anchor,
            kind: write_anchor_kind(rule),
        });
    }
    Ok(grants)
}

/// Materialize every granted-but-absent write anchor that falls inside a
/// trusted, disposable preparation root (the managed worktree).
///
/// A positive re-allow only becomes a bind mount if its anchor exists, so an
/// absent anchor silently leaves the path under the surrounding read-only
/// bind. Creating the anchor grants nothing the policy did not already grant —
/// the effective profile is the sole authority for *which* paths appear here.
pub fn prepare_linux_bwrap_write_grants(
    profile: &ResolvedFsProfile,
    containment_root: &Path,
) -> Result<PreparedWriteGrants, OrbitError> {
    let root = containment_root.canonicalize().map_err(|error| {
        OrbitError::Execution(format!(
            "canonicalize trusted write-grant preparation root `{}`: {error}",
            containment_root.display()
        ))
    })?;
    let mut prepared = PreparedWriteGrants::default();
    for grant in linux_bwrap_write_grants(profile)? {
        if ensure_write_anchor(&root, &grant, &mut prepared)? {
            prepared.created.push(grant.anchor);
        }
    }
    Ok(prepared)
}

/// Explain a path against the effective profile's write rules: `None` when the
/// path is granted, otherwise the deny that shadows it and the exception that
/// would be needed. This is what turns an EROFS into an attributable refusal.
pub fn linux_bwrap_write_grant_diagnostic(
    profile: &ResolvedFsProfile,
    path: &Path,
) -> Result<Option<String>, OrbitError> {
    match CompiledModifyRules::compile(profile)?.deciding_rule(path) {
        Some(rule) if !rule.starts_with('!') => Ok(None),
        Some(denied) => Ok(Some(format!(
            "`{}` is not writable inside the sandbox: denyModify rule `{}` shadows it and no later narrow re-allow grants it; add an exception such as `{}` to the effective policy",
            path.display(),
            denied,
            render_glob_path(path)
        ))),
        None => Ok(Some(format!(
            "`{}` is not writable inside the sandbox: no modify rule in fsProfile `{}` grants it",
            path.display(),
            profile.name
        ))),
    }
}

/// Returns `Ok(true)` when this call created the anchor. Records an
/// unsatisfied grant instead of creating anything outside `root`.
fn ensure_write_anchor(
    root: &Path,
    grant: &WriteGrant,
    prepared: &mut PreparedWriteGrants,
) -> Result<bool, OrbitError> {
    let Ok(relative) = grant.anchor.strip_prefix(root) else {
        return inspect_host_owned_anchor(root, grant, prepared);
    };

    // Validate the whole worktree-owned chain before consulting the final
    // target. `symlink_metadata(anchor)` follows intermediate symlinks, so an
    // existing outside target would otherwise bypass the absent-anchor checks.
    validate_owned_anchor_components(root, relative, grant)?;

    match std::fs::symlink_metadata(&grant.anchor) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(OrbitError::InvalidInput(format!(
                    "write-grant anchor `{}` (rule `{}`) must not be a symlink",
                    grant.anchor.display(),
                    grant.rule
                )));
            }
            let canonical = grant.anchor.canonicalize().map_err(|error| {
                OrbitError::InvalidInput(format!(
                    "write-grant anchor `{}` (rule `{}`) must resolve canonically inside `{}`: {error}",
                    grant.anchor.display(),
                    grant.rule,
                    root.display()
                ))
            })?;
            if !canonical.starts_with(root) {
                return Err(OrbitError::InvalidInput(format!(
                    "write-grant anchor `{}` (rule `{}`) resolves outside the trusted preparation root `{}` as `{}`",
                    grant.anchor.display(),
                    grant.rule,
                    root.display(),
                    canonical.display()
                )));
            }
            let matches = match grant.kind {
                WriteAnchorKind::File => metadata.is_file(),
                WriteAnchorKind::Directory => metadata.is_dir(),
            };
            if !matches {
                return Err(OrbitError::InvalidInput(format!(
                    "write-grant anchor `{}` (rule `{}`) exists with the wrong filesystem type",
                    grant.anchor.display(),
                    grant.rule
                )));
            }
            return Ok(false);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(OrbitError::Execution(format!(
                "inspect write-grant anchor `{}` (rule `{}`): {error}",
                grant.anchor.display(),
                grant.rule
            )));
        }
    }

    if let Some(parent) = grant.anchor.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            OrbitError::Execution(format!(
                "create write-grant anchor parent `{}` (rule `{}`): {error}",
                parent.display(),
                grant.rule
            ))
        })?;
        let canonical_parent = parent.canonicalize().map_err(|error| {
            OrbitError::InvalidInput(format!(
                "write-grant anchor parent `{}` (rule `{}`) must resolve canonically inside `{}`: {error}",
                parent.display(),
                grant.rule,
                root.display()
            ))
        })?;
        if !canonical_parent.starts_with(root) {
            return Err(OrbitError::InvalidInput(format!(
                "write-grant anchor parent `{}` (rule `{}`) resolves outside the trusted preparation root `{}` as `{}`",
                parent.display(),
                grant.rule,
                root.display(),
                canonical_parent.display()
            )));
        }
    }
    match grant.kind {
        WriteAnchorKind::File => {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&grant.anchor)
                .map_err(|error| {
                    OrbitError::Execution(format!(
                        "create write-grant anchor file `{}` (rule `{}`): {error}",
                        grant.anchor.display(),
                        grant.rule
                    ))
                })?;
        }
        WriteAnchorKind::Directory => {
            std::fs::create_dir(&grant.anchor).map_err(|error| {
                OrbitError::Execution(format!(
                    "create write-grant anchor directory `{}` (rule `{}`): {error}",
                    grant.anchor.display(),
                    grant.rule
                ))
            })?;
        }
    }
    Ok(true)
}

fn inspect_host_owned_anchor(
    root: &Path,
    grant: &WriteGrant,
    prepared: &mut PreparedWriteGrants,
) -> Result<bool, OrbitError> {
    match std::fs::symlink_metadata(&grant.anchor) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(OrbitError::InvalidInput(format!(
                    "write-grant anchor `{}` (rule `{}`) must not be a symlink",
                    grant.anchor.display(),
                    grant.rule
                )));
            }
            let matches = match grant.kind {
                WriteAnchorKind::File => metadata.is_file(),
                WriteAnchorKind::Directory => metadata.is_dir(),
            };
            if !matches {
                return Err(OrbitError::InvalidInput(format!(
                    "write-grant anchor `{}` (rule `{}`) exists with the wrong filesystem type",
                    grant.anchor.display(),
                    grant.rule
                )));
            }
            Ok(false)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            prepared.unsatisfied.push(UnsatisfiedWriteGrant {
                rule: grant.rule.clone(),
                anchor: grant.anchor.clone(),
                reason: format!(
                    "the anchor does not exist and lies outside the trusted preparation root `{}`, so the host must create it before dispatch",
                    root.display()
                ),
            });
            Ok(false)
        }
        Err(error) => Err(OrbitError::Execution(format!(
            "inspect write-grant anchor `{}` (rule `{}`): {error}",
            grant.anchor.display(),
            grant.rule
        ))),
    }
}

fn validate_owned_anchor_components(
    root: &Path,
    relative: &Path,
    grant: &WriteGrant,
) -> Result<(), OrbitError> {
    let mut current = root.to_path_buf();
    let component_count = relative.components().count();
    for (index, component) in relative.components().enumerate() {
        let std::path::Component::Normal(component) = component else {
            return Err(OrbitError::InvalidInput(format!(
                "write-grant anchor `{}` (rule `{}`) must not contain non-normal path components inside `{}`",
                grant.anchor.display(),
                grant.rule,
                root.display()
            )));
        };
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(OrbitError::InvalidInput(format!(
                    "write-grant anchor `{}` (rule `{}`) resolves through symlink `{}`; components inside the preparation root must not be a symlink",
                    grant.anchor.display(),
                    grant.rule,
                    current.display()
                )));
            }
            Ok(metadata) if index + 1 < component_count && !metadata.is_dir() => {
                return Err(OrbitError::InvalidInput(format!(
                    "write-grant anchor `{}` (rule `{}`) resolves through non-directory component `{}`",
                    grant.anchor.display(),
                    grant.rule,
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(OrbitError::Execution(format!(
                    "inspect write-grant anchor component `{}` (rule `{}`): {error}",
                    current.display(),
                    grant.rule
                )));
            }
        }
    }
    Ok(())
}

/// The policy grammar is the anchor-type contract: an exact rule denotes one
/// file, while `<root>/**` denotes a directory subtree. Filename punctuation
/// is never evidence, so extensionless files and dotted directories are both
/// represented without a hardcoded path inventory.
fn write_anchor_kind(rule: &str) -> WriteAnchorKind {
    if rule.ends_with("/**") {
        WriteAnchorKind::Directory
    } else {
        WriteAnchorKind::File
    }
}

/// The profile's `modify` rules compiled once, in rule order. Every
/// last-match-wins lookup a compile makes shares these regexes instead of
/// rebuilding one per rule per lookup.
pub(super) struct CompiledModifyRules<'a> {
    rules: Vec<(&'a str, Regex)>,
}

impl<'a> CompiledModifyRules<'a> {
    pub(super) fn compile(profile: &'a ResolvedFsProfile) -> Result<Self, OrbitError> {
        let rules = profile
            .modify
            .iter()
            .map(|rule| compile_rule_regex(rule).map(|regex| (rule.as_str(), regex)))
            .collect::<Result<_, _>>()?;
        Ok(Self { rules })
    }

    /// The rule that decides `path` under last-match-wins, if any matches.
    fn deciding_rule(&self, path: &Path) -> Option<&'a str> {
        let rendered = render_glob_path(path);
        self.rules
            .iter()
            .rev()
            .find(|(_, regex)| regex.is_match(&rendered))
            .map(|(rule, _)| *rule)
    }

    pub(super) fn grants_write(&self, path: &Path) -> bool {
        self.deciding_rule(path)
            .is_some_and(|rule| !rule.starts_with('!'))
    }
}

/// Compile a rule body (or a `!`-prefixed rule) into its glob regex.
pub(super) fn compile_rule_regex(rule: &str) -> Result<Regex, OrbitError> {
    let body = rule.strip_prefix('!').unwrap_or(rule);
    compile_glob_regex(body).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "invalid linux-bwrap filesystem glob `{body}`: {error}"
        ))
    })
}

pub(super) fn render_glob_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
