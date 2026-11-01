//! Bind a workspace's layout upgrades to the generation authority that
//! protects its live clients.
//!
//! Generation admission is per authority root (`~/.orbit`, an explicit
//! `--root` or `ORBIT_ROOT`), but a workspace `.orbit` is discovered from the
//! working directory independently of it. A process under a fresh authority
//! could therefore discover a workspace whose clients participate in another
//! authority, find nobody live in its own, and advance the workspace layout
//! under them. The layout is the one workspace-scoped part of a compatibility
//! identity, so the guard covers exactly the open that would migrate it.
//!
//! The workspace records the canonical authority protecting it in
//! `state/generation-authority.json` (gitignored runtime state that older
//! binaries never read). A pending layout migration is admitted only under
//! that authority. Nothing the opener supplies proves which authority that
//! is: the same path, a registration in the opener's own catalog, an absent
//! record or nobody live under the opener's authority are all true of an
//! unrelated authority too [ORB-20272]. So an open records an authority in
//! exactly one case, the first open of a workspace no layout-versioned binary
//! has stamped (layout versioning predates generation admission, so no
//! authority's clients can use it yet). Any other workspace that has
//! recorded none refuses a pending layout migration until an operator adopts
//! it with `orbit migrate --adopt-authority`
//! ([`adopt_workspace_layout_authority`]).
//!
//! Every writing open also lists its authority in
//! `state/generation-authorities.json`, the authorities known to have used
//! the workspace. A pending migration under the recorded authority, and an
//! adoption, hold admission exclusively on every other listed authority, so
//! neither proceeds while one of their clients is live: an authority that
//! merely registered the checkout cannot claim it, and the recorded one
//! cannot migrate it, under another authority's clients. Binaries from before
//! the ledger leave no entry, which is why the remedies still ask operators
//! to stop them.
//!
//! Record writes, ledger writes and every open that upgrades the layout hold
//! `state/generation-authority.lock` exclusively and decide again under it,
//! so concurrent first opens and adoptions are serialized. Admission on
//! other authorities is always taken before that fence. The record is
//! written before the layout changes and published whole (staged, then
//! hard-linked without replacement): a crash leaves either no record, with
//! the layout untouched, or the record, with the migration still pending
//! under it. An existing record is never replaced, and an unreadable or
//! malformed one refuses every pending migration and every adoption.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::fs::file_lock::{FileLockGuard, FileLockOptions, acquire_exclusive_file_lock};
use orbit_common::fs::generation::{GenerationUpdate, authority_root};
use orbit_store::workflow::layout::{
    SUPPORTED_LAYOUT_VERSION, current_layout_version, layout_ever_stamped,
};
use serde::{Deserialize, Serialize};

use crate::bootstrap::installed_binary::verify_installed_binary_keeps_state;

/// Record of the authority protecting a workspace, relative to its `.orbit`.
const AUTHORITY_RECORD: &str = "state/generation-authority.json";

/// Lock serializing record and ledger writes with the layout upgrades they
/// authorize, relative to the workspace `.orbit`.
const AUTHORITY_FENCE: &str = "state/generation-authority.lock";

/// Every authority known to have opened the workspace, relative to its
/// `.orbit`.
const AUTHORITY_LEDGER: &str = "state/generation-authorities.json";

/// The lock every generation participant holds, relative to an authority
/// root. Its presence shows a root has served as an authority.
const GENERATION_LOCK: &str = ".generation.lock";

#[derive(Debug, Serialize, Deserialize)]
struct AuthorityRecord {
    /// Canonical generation authority root whose participants use this
    /// workspace.
    authority: PathBuf,
    /// How the record was made, as its audit trail. Absent from records
    /// written before adoption existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recorded_by: Option<RecordProvenance>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RecordProvenance {
    /// `created` for the first open of a never-stamped workspace, `adopted`
    /// for `orbit migrate --adopt-authority`.
    method: String,
    /// What an adoption validated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evidence: Option<AuthorityAdoptionEvidence>,
    at: DateTime<Utc>,
    pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    executable: Option<PathBuf>,
    /// The workspace layout version when the record was written.
    layout_version: u32,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AuthorityLedger {
    authorities: BTreeSet<PathBuf>,
}

/// Why `orbit migrate --adopt-authority` accepted an authority's claim to a
/// workspace that recorded none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityAdoptionEvidence {
    /// The authority's workspace catalog registers the checkout.
    Registered,
    /// The workspace data directory is itself the explicit authority root.
    ExplicitRoot,
}

impl AuthorityAdoptionEvidence {
    /// The spelling the record and command output use.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::ExplicitRoot => "explicit_root",
        }
    }
}

/// Outcome of [`adopt_workspace_layout_authority`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceAuthorityAdoption {
    /// The workspace `.orbit` directory.
    pub orbit_dir: PathBuf,
    /// The canonical authority the workspace records.
    pub authority: PathBuf,
    pub evidence: AuthorityAdoptionEvidence,
    /// False when the workspace already recorded this authority, as after a
    /// retried adoption whose reply was lost.
    pub recorded: bool,
    /// The workspace layout version at adoption.
    pub layout_version: u32,
    /// Every authority the adoption held quiescent while it recorded.
    pub fenced: Vec<PathBuf>,
}

/// An admitted open. Holds what it fenced until the open has upgraded the
/// layout: the workspace fence, and admission on every other authority known
/// to use the workspace while a migration is pending.
#[must_use]
pub(crate) struct LayoutAuthorityAdmission {
    _others: Vec<GenerationUpdate>,
    _fence: Option<FileLockGuard>,
}

impl LayoutAuthorityAdmission {
    fn unfenced() -> Self {
        Self {
            _others: Vec::new(),
            _fence: None,
        }
    }
}

/// What the workspace's record says about this process's layout upgrade.
#[derive(Clone, Copy)]
enum Standing {
    /// Admitted; `pending` when it would migrate the layout.
    Admitted { pending: bool },
    /// The first open of a workspace no layout-versioned binary has stamped:
    /// admitted once it records this process's authority.
    Create,
}

/// Refuse a workspace layout upgrade that `global_root`'s generation authority
/// does not protect, or a layout or store schema migration the installed orbit
/// could not keep using, without writing anything.
///
/// Command surfaces call this before their own incidental writes; runtime
/// composition repeats it before bootstrap and records the authority.
pub fn verify_workspace_layout_authority(
    global_root: &Path,
    shared_root: &Path,
) -> Result<(), OrbitError> {
    verify_installed_binary_keeps_state(global_root, shared_root)?;
    layout_authority_standing(&authority_root(global_root)?, shared_root).map(|_| ())
}

/// [`verify_workspace_layout_authority`], then, for an open that may write
/// the layout or has not yet listed its authority, fence and decide again:
/// record the creator of a never-stamped workspace and list this authority
/// before the layout is written.
pub(crate) fn admit_workspace_layout_authority(
    global_root: &Path,
    shared_root: &Path,
) -> Result<LayoutAuthorityAdmission, OrbitError> {
    // A build that is not the installed orbit may not migrate past it, even
    // under the authority protecting the workspace [ORB-20262].
    verify_installed_binary_keeps_state(global_root, shared_root)?;
    let process = authority_root(global_root)?;
    let standing = layout_authority_standing(&process, shared_root)?;
    let pending = matches!(standing, Standing::Admitted { pending: true });
    if !pending && !matches!(standing, Standing::Create) && ledger_lists(shared_root, &process) {
        return Ok(LayoutAuthorityAdmission::unfenced());
    }
    // This process already participates in its own authority. Every other
    // one that has used the workspace must have no live client while the
    // layout changes under it.
    let (others, fenced) = if pending {
        fence_authorities(shared_root, &process, false, |authority, error| {
            refused(
                shared_root,
                current_layout_version(shared_root).unwrap_or_default(),
                format!(
                    "generation authority '{}', which has also used this workspace, still has \
                     clients this process's authority cannot protect: {error}",
                    authority.display()
                ),
                OTHER_AUTHORITY_REMEDY,
            )
        })?
    } else {
        (Vec::new(), BTreeSet::new())
    };
    let fence = match acquire_authority_fence(shared_root) {
        Ok(fence) => fence,
        // The layout upgrade cannot write here either, and nothing can be
        // recorded without the fence's directory.
        Err(error) if error.is_readonly_or_access_failure() => {
            tracing::warn!(
                target: "orbit.core.bootstrap",
                root = %shared_root.display(),
                error = %error,
                "skipped workspace generation authority fence"
            );
            return Ok(LayoutAuthorityAdmission {
                _others: others,
                _fence: None,
            });
        }
        Err(error) => return Err(error),
    };
    if pending && !ledger_within(shared_root, &process, &fenced) {
        return Err(refused(
            shared_root,
            current_layout_version(shared_root)?,
            "another generation authority opened it while this process fenced the ones it knew",
            RETRY_REMEDY,
        ));
    }
    record_created_authority(shared_root, &process)?;
    list_authority(shared_root, &process);
    Ok(LayoutAuthorityAdmission {
        _others: others,
        _fence: Some(fence),
    })
}

/// Under the fence, decide again and record this process's authority for a
/// never-stamped workspace that still records none.
fn record_created_authority(shared_root: &Path, process: &Path) -> Result<(), OrbitError> {
    // Once for this decision and once more should an older binary, which
    // records without the fence, publish a record between the two.
    for _ in 0..2 {
        if let Standing::Admitted { .. } = layout_authority_standing(process, shared_root)? {
            return Ok(());
        }
        match publish_authority_record(shared_root, process, "created", None) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                let error = OrbitError::from(error);
                if !error.is_readonly_or_access_failure() {
                    return Err(error);
                }
                // Unrecorded, the stamped workspace refuses every later
                // pending migration until it is adopted: still closed.
                tracing::warn!(
                    target: "orbit.core.bootstrap",
                    root = %shared_root.display(),
                    error = %error,
                    "skipped workspace generation authority record"
                );
                return Ok(());
            }
        }
    }
    Err(refused(
        shared_root,
        current_layout_version(shared_root)?,
        "its generation authority record kept changing while this process decided",
        RETRY_REMEDY,
    ))
}

/// Record `global_root`'s authority as the one protecting a workspace that
/// has recorded none, for `orbit migrate --adopt-authority`.
///
/// `claimed` must name this process's authority: an adoption records only
/// the authority it runs under. That authority's claim must be evidenced, by
/// its catalog registering the checkout (`registered`, which the caller looks
/// up) or by the workspace data directory being the explicit root itself.
/// Admission is then held exclusively on it, on every authority the
/// workspace's ledger lists and on the data directory when it has served as
/// an authority: each refuses while any of its long-lived participants is
/// live (naming them) and waits for short-lived ones, so none of their
/// clients is live while the record is written. A workspace recording this
/// authority already is a successful no-op; any other record, readable or
/// not, is never replaced.
pub fn adopt_workspace_layout_authority(
    global_root: &Path,
    shared_root: &Path,
    claimed: &Path,
    registered: bool,
) -> Result<WorkspaceAuthorityAdoption, OrbitError> {
    let process = authority_root(global_root)?;
    let claimed = authority_root(claimed)?;
    if claimed != process {
        return Err(adoption_refused(
            shared_root,
            format!(
                "it names generation authority '{}', but this process runs under '{}'; an \
                 adoption records only its own authority, so run it under the authority it \
                 names (the same home directory, `--root` or ORBIT_ROOT)",
                claimed.display(),
                process.display()
            ),
        ));
    }
    let explicit_root = shared_root
        .canonicalize()
        .is_ok_and(|shared| shared == process);
    let evidence = if explicit_root {
        AuthorityAdoptionEvidence::ExplicitRoot
    } else if registered {
        AuthorityAdoptionEvidence::Registered
    } else {
        return Err(adoption_refused(
            shared_root,
            format!(
                "generation authority '{}' has not registered this checkout and is not the \
                 workspace data directory, so nothing shows its clients use the workspace",
                process.display()
            ),
        ));
    };
    let (_admissions, fenced) =
        fence_authorities(shared_root, &process, true, |authority, error| {
            adoption_refused(
                shared_root,
                format!(
                    "generation authority '{}', which {}, could not be fenced: {error}",
                    authority.display(),
                    if authority == process {
                        "this adoption runs under"
                    } else {
                        "has used this workspace"
                    }
                ),
            )
        })?;
    let _fence = acquire_authority_fence(shared_root)?;
    if !ledger_within(shared_root, &process, &fenced) {
        return Err(adoption_refused(
            shared_root,
            "another generation authority opened it while the adoption fenced the ones it \
             knew; re-run the adoption",
        ));
    }
    let layout_version = current_layout_version(shared_root)?;
    let adoption = |recorded| WorkspaceAuthorityAdoption {
        orbit_dir: shared_root.to_path_buf(),
        authority: process.clone(),
        evidence,
        recorded,
        layout_version,
        fenced: fenced.iter().cloned().collect(),
    };
    for _ in 0..2 {
        match read_authority_record(shared_root) {
            Ok(Some(recorded)) => {
                return if authority_root(&recorded).is_ok_and(|recorded| recorded == process) {
                    Ok(adoption(false))
                } else {
                    Err(adoption_refused(
                        shared_root,
                        format!(
                            "it already records generation authority '{}', which an adoption \
                             never replaces",
                            recorded.display()
                        ),
                    ))
                };
            }
            Ok(None) => {}
            Err(detail) => {
                return Err(adoption_refused(
                    shared_root,
                    format!("{detail}, which an adoption never replaces"),
                ));
            }
        }
        match publish_authority_record(shared_root, &process, "adopted", Some(evidence)) {
            Ok(()) => {
                list_authority(shared_root, &process);
                tracing::info!(
                    target: "orbit.core.bootstrap",
                    root = %shared_root.display(),
                    authority = %process.display(),
                    evidence = evidence.as_str(),
                    layout_version,
                    "adopted workspace generation authority"
                );
                return Ok(adoption(true));
            }
            // An older binary, which records without the fence, published
            // first: decide from its record.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(adoption_refused(
        shared_root,
        "its generation authority record kept changing during the adoption",
    ))
}

fn layout_authority_standing(process: &Path, shared_root: &Path) -> Result<Standing, OrbitError> {
    let version = current_layout_version(shared_root)?;
    // Version 0 is pending too: every migration from the baseline applies.
    let pending = version < SUPPORTED_LAYOUT_VERSION;
    match read_authority_record(shared_root) {
        Ok(Some(_)) if !pending => Ok(Standing::Admitted { pending }),
        Ok(Some(recorded)) => {
            if authority_root(&recorded)? == process {
                return Ok(Standing::Admitted { pending });
            }
            Err(refused(
                shared_root,
                version,
                format!(
                    "its clients are protected by generation authority '{}', not this \
                     process's '{}'",
                    recorded.display(),
                    process.display()
                ),
                RECORDED_REMEDY,
            ))
        }
        Ok(None) if version == 0 && !layout_ever_stamped(shared_root) => Ok(Standing::Create),
        Ok(None) if !pending => Ok(Standing::Admitted { pending }),
        Ok(None) => Err(refused(
            shared_root,
            version,
            format!(
                "it has recorded no generation authority, so nothing shows that this \
                 process's authority '{}' protects its clients",
                process.display()
            ),
            ADOPTION_REMEDY,
        )),
        Err(detail) if pending => Err(refused(shared_root, version, detail, UNUSABLE_REMEDY)),
        Err(_) => Ok(Standing::Admitted { pending }),
    }
}

/// Hold admission exclusively, in path order, on every authority the ledger
/// lists and on the workspace data directory when it has served as an
/// authority itself, and on `process` too when `including_process`. Only
/// roots that have served as an authority are fenced: fencing creates the
/// admission files, and nobody can be live under a root that has none.
/// `unfenced` builds the error naming an authority that could not be fenced.
fn fence_authorities(
    shared_root: &Path,
    process: &Path,
    including_process: bool,
    unfenced: impl Fn(&Path, OrbitError) -> OrbitError,
) -> Result<(Vec<GenerationUpdate>, BTreeSet<PathBuf>), OrbitError> {
    let authorities = authorities_to_fence(shared_root, process, including_process, &unfenced)?;
    let admissions = authorities
        .iter()
        .map(|authority| {
            GenerationUpdate::acquire(authority).map_err(|error| unfenced(authority, error))
        })
        .collect::<Result<_, _>>()?;
    Ok((admissions, authorities))
}

/// The authorities [`fence_authorities`] holds admission on, canonical.
fn authorities_to_fence(
    shared_root: &Path,
    process: &Path,
    including_process: bool,
    unfenced: &impl Fn(&Path, OrbitError) -> OrbitError,
) -> Result<BTreeSet<PathBuf>, OrbitError> {
    let ledger = read_authority_ledger(shared_root).map_err(|detail| {
        unfenced(
            shared_root,
            OrbitError::Execution(format!(
                "{detail}, so it cannot show which authorities use the workspace"
            )),
        )
    })?;
    let mut candidates = ledger.authorities;
    candidates.insert(shared_root.to_path_buf());
    if including_process {
        candidates.insert(process.to_path_buf());
    }
    // One spelling per authority: flock refuses a second lock on the same
    // file, even from this process.
    let mut authorities = BTreeSet::new();
    for candidate in candidates {
        let served = candidate.join(GENERATION_LOCK).is_file();
        if !served && !(including_process && candidate == process) {
            continue;
        }
        let authority = authority_root(&candidate).map_err(|error| unfenced(&candidate, error))?;
        if including_process || authority != process {
            authorities.insert(authority);
        }
    }
    Ok(authorities)
}

/// The generation authorities, other than `global_root`'s own, that an open
/// under `global_root` holds admission on exclusively while it migrates the
/// layout of `shared_root` to `target_layout`: empty when the workspace is
/// already at or past it.
///
/// `orbit update` pins the candidate generation in every authority it
/// admitted against, and keeps those pins until convergence ends. Its
/// `migrate --confirm` child cannot fence an authority the updater still
/// pins, and an unregistered shared hold is indistinguishable from a foreign
/// client. The updater therefore hands exactly these authorities to the
/// migration's fence: it releases its pin on them before the step, and the
/// fence takes them exclusively, refusing if anything joined in between.
pub fn layout_migration_fenced_authorities(
    global_root: &Path,
    shared_root: &Path,
    target_layout: u32,
) -> Result<BTreeSet<PathBuf>, OrbitError> {
    if current_layout_version(shared_root)? >= target_layout {
        return Ok(BTreeSet::new());
    }
    let process = authority_root(global_root)?;
    authorities_to_fence(shared_root, &process, false, &|_, error| error)
}

/// Whether every authority the ledger lists now, other than `process`, is
/// among `fenced` or has never served as an authority. An unusable ledger
/// is not.
fn ledger_within(shared_root: &Path, process: &Path, fenced: &BTreeSet<PathBuf>) -> bool {
    read_authority_ledger(shared_root).is_ok_and(|ledger| {
        ledger.authorities.iter().all(|authority| {
            authority == process
                || fenced.contains(authority)
                || !authority.join(GENERATION_LOCK).is_file()
        })
    })
}

fn acquire_authority_fence(shared_root: &Path) -> Result<FileLockGuard, OrbitError> {
    acquire_exclusive_file_lock(
        &shared_root.join(AUTHORITY_FENCE),
        "workspace generation authority",
        FileLockOptions::default(),
    )
    .map_err(OrbitError::from)
}

/// Whether the ledger lists `process`. An unusable ledger counts as listing
/// it: it is never rewritten, and fencing refuses on it.
fn ledger_lists(shared_root: &Path, process: &Path) -> bool {
    read_authority_ledger(shared_root).map_or(true, |ledger| ledger.authorities.contains(process))
}

/// Under the fence, add `process` to the ledger. Best effort: a workspace
/// that cannot list it keeps opening, and fencing then misses that
/// authority, as it misses a binary from before the ledger.
fn list_authority(shared_root: &Path, process: &Path) {
    let Ok(mut ledger) = read_authority_ledger(shared_root) else {
        return;
    };
    if !ledger.authorities.insert(process.to_path_buf()) {
        return;
    }
    let path = shared_root.join(AUTHORITY_LEDGER);
    let written = serde_json::to_string(&ledger)
        .map_err(std::io::Error::other)
        .and_then(|content| {
            orbit_common::fs::io::atomic_write_text(&path, &format!("{content}\n"))
        });
    if let Err(error) = written {
        tracing::warn!(
            target: "orbit.core.bootstrap",
            path = %path.display(),
            error = %error,
            "skipped workspace generation authority ledger"
        );
    }
}

/// The authorities that have opened the workspace, and an error detail when
/// the ledger exists but cannot be used.
fn read_authority_ledger(shared_root: &Path) -> Result<AuthorityLedger, String> {
    let path = shared_root.join(AUTHORITY_LEDGER);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AuthorityLedger::default());
        }
        Err(error) => {
            return Err(format!(
                "its generation authority ledger '{}' is unreadable: {error}",
                path.display()
            ));
        }
    };
    match serde_json::from_str::<AuthorityLedger>(raw.trim()) {
        Ok(ledger) if ledger.authorities.iter().all(|path| path.is_absolute()) => Ok(ledger),
        Ok(_) => Err(format!(
            "its generation authority ledger '{}' names a relative path",
            path.display()
        )),
        Err(error) => Err(format!(
            "its generation authority ledger '{}' is malformed: {error}",
            path.display()
        )),
    }
}

/// Publish the record whole, failing with `AlreadyExists` rather than
/// replacing one.
fn publish_authority_record(
    shared_root: &Path,
    authority: &Path,
    method: &str,
    evidence: Option<AuthorityAdoptionEvidence>,
) -> std::io::Result<()> {
    let record = AuthorityRecord {
        authority: authority.to_path_buf(),
        recorded_by: Some(RecordProvenance {
            method: method.to_string(),
            evidence,
            at: Utc::now(),
            pid: std::process::id(),
            executable: std::env::current_exe().ok(),
            layout_version: current_layout_version(shared_root).map_err(std::io::Error::other)?,
        }),
    };
    let content = serde_json::to_string(&record).map_err(std::io::Error::other)?;
    orbit_common::fs::io::write_new_private_text(
        &shared_root.join(AUTHORITY_RECORD),
        &format!("{content}\n"),
    )
}

/// The recorded authority, `None` when the workspace has recorded none, and
/// an error detail when the record exists but cannot be used.
fn read_authority_record(shared_root: &Path) -> Result<Option<PathBuf>, String> {
    let path = shared_root.join(AUTHORITY_RECORD);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "its generation authority record '{}' is unreadable: {error}",
                path.display()
            ));
        }
    };
    match serde_json::from_str::<AuthorityRecord>(raw.trim()) {
        Ok(record) if record.authority.is_absolute() => Ok(Some(record.authority)),
        Ok(_) => Err(format!(
            "its generation authority record '{}' does not name an absolute path",
            path.display()
        )),
        Err(error) => Err(format!(
            "its generation authority record '{}' is malformed: {error}",
            path.display()
        )),
    }
}

const RECORDED_REMEDY: &str = "Run the command under the generation authority its clients use \
     (the same home directory, `--root` or ORBIT_ROOT), where admission waits for them to \
     quiesce";

const ADOPTION_REMEDY: &str = "Stop every orbit process that uses this workspace, under any \
     home directory, `--root` or ORBIT_ROOT, then run `orbit migrate --adopt-authority \
     <authority>` under the authority they use and re-run";

const OTHER_AUTHORITY_REMEDY: &str = "Stop that authority's orbit processes that use this \
     workspace and re-run; this process's authority cannot wait for them";

const UNUSABLE_REMEDY: &str = "Orbit never replaces the record: restore it to name the \
     generation authority this workspace's clients use, as `{\"authority\":\"<absolute \
     path>\"}`, and re-run under that authority";

const RETRY_REMEDY: &str = "Re-run the command";

fn refused(
    shared_root: &Path,
    version: u32,
    detail: impl std::fmt::Display,
    remedy: &str,
) -> OrbitError {
    OrbitError::Execution(format!(
        "upgrade admission refused: workspace '{}' has a pending layout upgrade \
         (v{version} to v{SUPPORTED_LAYOUT_VERSION}) but {detail}; leave the installation and \
         stores unchanged. {remedy}. Do not delete admission files, authority records or \
         layout markers",
        shared_root.display()
    ))
}

fn adoption_refused(shared_root: &Path, detail: impl std::fmt::Display) -> OrbitError {
    OrbitError::Execution(format!(
        "workspace authority adoption refused: workspace '{}' was left unchanged because \
         {detail}. Do not delete admission files, authority records or layout markers",
        shared_root.display()
    ))
}
