use super::*;

/// Where a refresh reloads the servable workspace set from. Present only in the
/// registry-backed mode built by [`crate::serve::build_state`]; [`DashboardState::single`]
/// and [`DashboardState::global`] leave it `None`, making [`DashboardState::refresh`]
/// a no-op (their entries are supplied directly and never re-read).
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
    /// Successful `load` calls since construction. Test-only: production never
    /// reads this; it exists so tests can prove the request-path freshness gate
    /// skipped I/O.
    #[cfg(test)]
    pub(super) load_count: AtomicU64,
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
            #[cfg(test)]
            load_count: AtomicU64::new(0),
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
    pub(super) fn load(&self) -> Result<SnapshotData, OrbitError> {
        #[cfg(test)]
        self.load_count.fetch_add(1, Ordering::Relaxed);
        let mut registry = workspace_registry::load_registry_from(&self.registry_path)?;
        workspace_registry::validate_workspaces(&mut registry);
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
        let default_workspace = crate::default_workspace_selection(
            &registry,
            self.workspace_selector.as_deref(),
            self.cwd.as_deref(),
        )
        .filter(|id| entries.iter().any(|entry| entry.id == *id && entry.active));
        Ok(SnapshotData {
            entries,
            default_workspace,
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
