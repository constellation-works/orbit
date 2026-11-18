use super::*;

use orbit_cmd::registry_runtime;
use orbit_types::workspace::WorkspaceRegistry;

/// Where a refresh reloads the servable workspace set from. Present only in the
/// registry-backed mode built by [`crate::serve::build_state`].
/// [`DashboardState::single`] leaves it `None`, making
/// [`DashboardState::refresh`] a no-op for its pre-built runtime.
pub(crate) struct RegistrySource {
    /// Path to the served registry: `<--root>/workspaces.json` when an
    /// explicit root was given, `~/.orbit/workspaces.json` otherwise (or a
    /// test double).
    pub(super) registry_path: PathBuf,
    /// The `--workspace <selector>` flag, if any, for default re-selection.
    workspace_selector: Option<String>,
    /// Process cwd captured at startup, for default re-selection.
    cwd: Option<PathBuf>,
    /// Per-checkout failures already reported to operators. A refresh may retry
    /// on later request boundaries when the registry or a checkout fingerprint
    /// changes, so retaining this set avoids emitting the same diagnostic for
    /// every reload while allowing a repaired-and-broken-again checkout to be
    /// reported anew.
    reported_unavailable: Mutex<HashSet<UnavailableCheckout>>,
}

/// A parsed registry view and the file identity observed before its read.
pub(super) struct LoadedRegistry {
    /// Workspace data derived from the registry read.
    pub(super) data: SnapshotData,
    /// Sampled before the read: a later rewrite cannot make old data appear
    /// current, even if it happens before this snapshot is published.
    pub(super) fingerprint: Option<RegistryFingerprint>,
}

/// `stat` identity of `workspaces.json`. Equal fingerprints mean the registry
/// file has not been rewritten, so `load` would observe the same bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RegistryFingerprint {
    mtime: SystemTime,
    len: u64,
}

/// `stat` identity for one filesystem path used to validate a registered
/// checkout. `None` deliberately participates in the identity so a checkout
/// that disappears, or an invalid checkout that is repaired, triggers the
/// next request-path reload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileFingerprint {
    mtime: SystemTime,
    len: u64,
}

impl FileFingerprint {
    fn read(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        Some(Self {
            mtime: metadata.modified().ok()?,
            len: metadata.len(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CheckoutFingerprint {
    id: String,
    repo_root: PathBuf,
    orbit_dir: PathBuf,
    repo_root_stat: Option<FileFingerprint>,
    orbit_dir_stat: Option<FileFingerprint>,
    config_stat: Option<FileFingerprint>,
}

impl CheckoutFingerprint {
    fn read(entry: &WsEntry) -> Self {
        Self {
            id: entry.id.clone(),
            repo_root: entry.repo_root.clone(),
            orbit_dir: entry.orbit_dir.clone(),
            repo_root_stat: FileFingerprint::read(&entry.repo_root),
            orbit_dir_stat: FileFingerprint::read(&entry.orbit_dir),
            config_stat: FileFingerprint::read(&entry.orbit_dir.join("config.yaml")),
        }
    }
}

pub(super) fn checkout_fingerprints(entries: &[WsEntry]) -> Vec<CheckoutFingerprint> {
    entries.iter().map(CheckoutFingerprint::read).collect()
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct UnavailableCheckout {
    workspace: String,
    checkout: PathBuf,
    error: String,
}

impl UnavailableCheckout {
    fn warn(&self) {
        tracing::warn!(
            workspace = %self.workspace,
            checkout = %self.checkout.display(),
            error = %self.error,
            "registered workspace is unavailable; dashboard will continue serving healthy workspaces"
        );
    }
}

impl RegistrySource {
    pub(crate) fn new(
        registry_path: PathBuf,
        workspace_selector: Option<String>,
        cwd: Option<PathBuf>,
    ) -> Self {
        Self {
            registry_path,
            workspace_selector,
            cwd,
            reported_unavailable: Mutex::new(HashSet::new()),
        }
    }

    /// `stat` mtime and length of the registry file. `None` if the path cannot
    /// be observed, which forces a reload so a vanished or unreadable file is
    /// not sticky behind a stale fingerprint.
    pub(super) fn fingerprint(&self) -> Option<RegistryFingerprint> {
        let (mtime, len) = workspace_registry::registry_file_fingerprint(&self.registry_path)?;
        Some(RegistryFingerprint { mtime, len })
    }

    /// Reload the authoritative registry into a fresh (generation-less) snapshot
    /// view. Stale-path workspaces are marked inactive (never deleted) via
    /// `validate_workspaces`; active checkouts whose identity cannot be resolved
    /// are likewise excluded after an operator-visible warning. The caller stamps
    /// the generation at publication.
    pub(super) fn load(&self) -> Result<LoadedRegistry, OrbitError> {
        let fingerprint = self.fingerprint();
        let registry = workspace_registry::load_validated_registry_from(
            &self.registry_path,
            workspace_registry::WorkspaceValidationMode::InMemory,
        )?;
        let mut unavailable = HashSet::new();
        let entries: Vec<WsEntry> = workspace_registry::local_workspaces(&registry)
            .map(|(workspace, checkout)| {
                let binding = (workspace.status == WorkspaceStatus::Active)
                    .then(|| workspace_runtime_binding(workspace, checkout))
                    .and_then(|result| match result {
                        Ok(binding) => Some(binding),
                        Err(error) => {
                            unavailable.insert(UnavailableCheckout {
                                workspace: workspace.id.clone(),
                                checkout: checkout.orbit_dir.clone(),
                                error: error.to_string(),
                            });
                            None
                        }
                    });
                let active = binding.is_some();
                WsEntry {
                    id: workspace.id.clone(),
                    name: workspace.name.clone(),
                    repo_root: checkout.repo_root.clone(),
                    orbit_dir: checkout.orbit_dir.clone(),
                    binding,
                    active,
                }
            })
            .collect();
        self.report_unavailable(unavailable);
        let default_workspace = default_workspace_selection(
            &registry,
            self.workspace_selector.as_deref(),
            self.cwd.as_deref(),
        )
        .filter(|id| entries.iter().any(|entry| entry.id == *id && entry.active));
        Ok(LoadedRegistry {
            data: SnapshotData {
                entries,
                default_workspace,
            },
            fingerprint,
        })
    }

    fn report_unavailable(&self, unavailable: HashSet<UnavailableCheckout>) {
        let new_failures = {
            let mut reported = self
                .reported_unavailable
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            reported.retain(|failure| unavailable.contains(failure));
            unavailable
                .into_iter()
                .filter(|failure| reported.insert(failure.clone()))
                .collect::<Vec<_>>()
        };
        for failure in new_failures {
            failure.warn();
        }
    }
}

/// Best-effort default for `cwd` when serving globally: the `Active` registered
/// workspace whose repo root or path override is the longest prefix of `cwd`,
/// if the server was launched inside one. The match is lexical
/// (`Path::starts_with`) and does not touch the filesystem. An `Invalid`
/// checkout candidate is never eligible, but excluding it does not exclude its
/// descendants: a `cwd` beneath an invalid checkout still matches an eligible
/// enclosing root or path override if one exists, and yields `None` only when no
/// eligible enclosing root or override contains it. `None` is no hint, not "All
/// workspaces"; see `default_workspace_selection` for what the browser does
/// next.
pub(crate) fn default_workspace_for_cwd(
    registry: &WorkspaceRegistry,
    cwd: &Path,
) -> Option<String> {
    workspace_registry::local_workspaces(registry)
        .filter(|(workspace, _)| workspace.status == WorkspaceStatus::Active)
        .filter_map(|(workspace, checkout)| {
            std::iter::once(&checkout.repo_root)
                .chain(&checkout.path_overrides)
                .filter(|candidate| cwd.starts_with(candidate))
                .map(|candidate| candidate.as_os_str().len())
                .max()
                .map(|prefix_len| (workspace, prefix_len))
        })
        .max_by_key(|(_, prefix_len)| *prefix_len)
        .map(|(workspace, _)| workspace.id.clone())
}

/// Precedence logic for the registry's default-workspace hint. An explicit
/// `workspace_selector` (the `--workspace` flag) always wins over `cwd` when
/// given, even when it resolves to nothing: the result is then `None`, not a
/// cwd-based default. An explicit but unresolved bare name never reaches the
/// path match, so it cannot preselect the cwd's workspace. Only an absent
/// selector (no `--workspace` flag) falls back to the cwd match
/// ([`default_workspace_for_cwd`]).
///
/// A selector first resolves as a registered name or logical `ws_*` ID to an
/// `Active` workspace. Otherwise a path-shaped selector
/// ([`registry_runtime::selector_looks_like_path`]) is resolved through
/// [`resolve_selector_path`] and matched the way a cwd is. That is what
/// `orbit web connect` forwards for a remote workspace directory.
///
/// The result is only a hint, and `None` is not an "All workspaces" choice.
/// This function checks registry status only. `RegistrySource::load` then keeps
/// the hint only when the workspace's runtime binding succeeds, and the browser
/// decides what opens (see `build_state`).
pub(crate) fn default_workspace_selection(
    registry: &WorkspaceRegistry,
    workspace_selector: Option<&str>,
    cwd: Option<&Path>,
) -> Option<String> {
    match workspace_selector {
        Some(selector) => workspace_registry::resolve_logical_workspace(registry, selector)
            .ok()
            .filter(|workspace| workspace.status == WorkspaceStatus::Active)
            .map(|workspace| workspace.id.clone())
            .or_else(|| {
                if !registry_runtime::selector_looks_like_path(selector) {
                    return None;
                }
                let path = resolve_selector_path(Path::new(selector), cwd);
                default_workspace_for_cwd(registry, &path)
            }),
        None => cwd.and_then(|cwd| default_workspace_for_cwd(registry, cwd)),
    }
}

/// Normalize a path-shaped `--workspace <selector>` value so it can be
/// prefix-matched against eligible registered roots. Those roots are canonical
/// absolute paths after the pipeline's canonicalization (see
/// `orbit-runtime/src/builder.rs`).
///
/// Relative paths are resolved against `cwd` before canonicalization. `cwd` is
/// `None` when the process has no usable working directory (`build_state`
/// passes `current_dir().ok()`). A relative selector then stays relative and
/// cannot prefix-match an absolute root, so it yields no hint. If
/// canonicalization fails (the path may not exist, or symlink resolution
/// errors), the absolute lexical path is returned so it can still be matched
/// component-wise by [`default_workspace_for_cwd`]. A nonexistent descendant of
/// an eligible root (e.g. `/registered/missing` under `/registered`) therefore
/// still selects that root. Only a path lexically outside every eligible root
/// misses. An `Invalid` checkout candidate is not eligible, but excluding it
/// does not exclude its descendants: a path beneath an invalid checkout still
/// matches an eligible enclosing root or path override if one exists, and misses
/// only when no eligible enclosing root or override contains it. Lexical
/// matching does not resolve `..` components, so a nonexistent
/// `/registered/../other` still matches `/registered` as written.
fn resolve_selector_path(selector: &Path, cwd: Option<&Path>) -> PathBuf {
    let absolute = if selector.is_absolute() {
        selector.to_path_buf()
    } else {
        match cwd {
            Some(cwd) => cwd.join(selector),
            None => selector.to_path_buf(),
        }
    };
    absolute.canonicalize().unwrap_or(absolute)
}
