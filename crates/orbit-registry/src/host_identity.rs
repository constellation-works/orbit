//! Host identity for this Orbit machine [ORB-10247, ORB-10721].
//!
//! `~/.orbit/host.toml` carries the one genuinely host-local datum: a versioned
//! [`HostIdentity`] with a stable, generated `machine_id`, an operator-chosen
//! `host_id` display name, and an immutable task namespace. First-time creation
//! lives in the global `orbit init` flow; legacy identities
//! are migrated in place once, preserving their existing machine identity and
//! seeding the historical `ORB` task namespace.
//!
//! Loading is strict: after migration an absent, malformed, incomplete, blank,
//! or future-schema file is a hard error with an actionable message — there is
//! no silent fallback to the OS hostname. Routine `hosts:` pinning,
//! the sweep, and status all resolve through [`HostIdentity::host_id`].

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_text, with_exclusive_file_lock};
use orbit_common::protocol::toml::escape_basic_string;
use orbit_types::identity::{MACHINE_ID_PREFIX, validate_machine_id};
use serde::Deserialize;

/// File under the global Orbit root carrying the host identity.
pub const HOST_TOML_FILE: &str = "host.toml";
/// Current on-disk host identity schema version.
pub const HOST_IDENTITY_SCHEMA_VERSION: u32 = 2;
const LEGACY_TASK_PREFIX: &str = "ORB";
const RESERVED_TASK_PREFIXES: [&str; 4] = ["ORB", "ADR", "L", "F"];

/// Validate an operator's fresh task-prefix choice.
///
/// The persisted migration prefix `ORB` remains valid for existing machines,
/// but cannot be selected for a new identity.
pub fn validate_new_task_prefix(value: &str) -> Result<String, OrbitError> {
    if RESERVED_TASK_PREFIXES.contains(&value) {
        return Err(OrbitError::InvalidInput(format!(
            "task prefix '{value}' is reserved; choose 2-5 uppercase ASCII letters other than ORB, ADR, L, or F"
        )));
    }
    if !(2..=5).contains(&value.len()) || !value.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err(OrbitError::InvalidInput(
            "task prefix must be 2-5 uppercase ASCII letters".to_string(),
        ));
    }
    Ok(value.to_string())
}

fn validate_stored_task_prefix(value: &str) -> Result<String, OrbitError> {
    if value == LEGACY_TASK_PREFIX {
        return Ok(value.to_string());
    }
    validate_new_task_prefix(value)
}

/// Versioned machine identity persisted in `~/.orbit/host.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostIdentity {
    /// On-disk schema version; always [`HOST_IDENTITY_SCHEMA_VERSION`] for a
    /// value produced by this build.
    pub schema_version: u32,
    /// Opaque, generated-once, never-reused stable identity (`hm_<hex>`).
    pub machine_id: String,
    /// Operator-chosen, renameable display name; matched against routine
    /// `hosts:` pins.
    pub host_id: String,
    /// Immutable namespace for task ids minted by this machine.
    pub task_prefix: String,
}

impl HostIdentity {
    /// Deterministic `host.toml` rendering — same identity always serializes to
    /// the same bytes, so a re-migration or repeated init is a no-op write.
    /// Operator-influenced string values are escaped for a TOML basic string so
    /// a quote, backslash, or control character cannot corrupt the file.
    fn to_toml(&self) -> String {
        // ORB-10721: source provenance for the generated host identity format.
        format!(
            "# Machine identity for this Orbit host. Created by\n\
             # `orbit init`; `machine_id` is generated once and never edited.\n\
             # `host_id` is the operator-chosen display name matched against\n\
             # routine `hosts:` pins; `task_prefix` is chosen once.\n\
             schema_version = {}\n\
             machine_id = \"{}\"\n\
             host_id = \"{}\"\n\
             task_prefix = \"{}\"\n",
            self.schema_version,
            escape_basic_string(&self.machine_id),
            escape_basic_string(&self.host_id),
            escape_basic_string(&self.task_prefix),
        )
    }
}

/// The three actionable states of `host.toml`. Malformed, incomplete, blank,
/// and future-schema files are returned as `Err` by [`inspect_host_identity`],
/// never as a variant here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostIdentityState {
    /// A complete, current-schema identity.
    Present(HostIdentity),
    /// A legacy pre-migration identity (host-id-only or schema v1).
    Legacy {
        /// The preserved, non-blank legacy `host_id`.
        host_id: String,
        /// A schema-v1 machine id, preserved when present. The oldest
        /// host-id-only format has none and receives one during migration.
        machine_id: Option<String>,
    },
    /// No `host.toml` exists yet.
    Absent,
}

impl HostIdentityState {
    /// The host name this state resolves to for routine pinning, if any.
    pub fn host_id(&self) -> Option<&str> {
        match self {
            HostIdentityState::Present(identity) => Some(&identity.host_id),
            HostIdentityState::Legacy { host_id, .. } => Some(host_id),
            HostIdentityState::Absent => None,
        }
    }
}

/// Outcome of [`ensure_host_identity`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostIdentityOutcome {
    /// A fresh identity was created from operator input.
    Created(HostIdentity),
    /// A legacy identity was migrated to the current schema.
    Migrated(HostIdentity),
    /// A complete current-schema identity already existed; nothing was written.
    Unchanged(HostIdentity),
}

impl HostIdentityOutcome {
    /// The resulting identity, regardless of how it was reached.
    pub fn identity(&self) -> &HostIdentity {
        match self {
            HostIdentityOutcome::Created(identity)
            | HostIdentityOutcome::Migrated(identity)
            | HostIdentityOutcome::Unchanged(identity) => identity,
        }
    }
}

/// Operator-supplied fields for a first-time identity creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewHostIdentity {
    /// Operator-chosen display name.
    pub host_id: String,
    /// Operator-chosen task namespace.
    pub task_prefix: String,
}

#[derive(Debug, Deserialize)]
struct RawHostToml {
    schema_version: Option<u32>,
    machine_id: Option<String>,
    host_id: Option<String>,
    task_prefix: Option<String>,
    #[serde(rename = "mode")]
    _mode: Option<String>,
}

fn host_toml_path(global_root: &Path) -> PathBuf {
    global_root.join(HOST_TOML_FILE)
}

fn existing_host_toml_path(global_root: &Path) -> Result<Option<PathBuf>, OrbitError> {
    let canonical_root = match global_root.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "failed to canonicalize host identity directory '{}': {error}",
                global_root.display()
            )));
        }
    };
    let canonical_path = canonical_root.join(HOST_TOML_FILE);

    match std::fs::symlink_metadata(&canonical_path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(OrbitError::InvalidInput(format!(
                "host identity path must be a regular {HOST_TOML_FILE} file inside '{}': {}",
                global_root.display(),
                canonical_path.display()
            )))
        }
        Ok(_) => Ok(Some(canonical_path)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(OrbitError::Io(format!(
            "failed to inspect host identity '{}': {error}",
            canonical_path.display()
        ))),
    }
}

fn non_blank(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Classify `host.toml` without mutating it. Returns the actionable
/// [`HostIdentityState`] for absent / legacy / present files, and a hard error
/// for malformed, incomplete, blank, or future-schema files (fail closed —
/// never rewrites the file).
pub fn inspect_host_identity(global_root: &Path) -> Result<HostIdentityState, OrbitError> {
    let Some(path) = existing_host_toml_path(global_root)? else {
        return Ok(HostIdentityState::Absent);
    };
    let raw_text = std::fs::read_to_string(&path)
        .map_err(|error| OrbitError::Io(format!("failed to read '{}': {error}", path.display())))?;
    let parsed: RawHostToml = toml::from_str(&raw_text).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "invalid host identity '{}': {error}",
            path.display()
        ))
    })?;

    match parsed.schema_version {
        None => match non_blank(&parsed.host_id) {
            Some(host_id) => {
                let machine_id =
                    validated_optional_machine_id(&path, parsed.machine_id.as_deref())?;
                Ok(HostIdentityState::Legacy {
                    host_id,
                    machine_id,
                })
            }
            None => Err(OrbitError::InvalidInput(format!(
                "host identity '{}' is incomplete: no schema_version and no host_id; \
                 run `orbit init` to create one",
                path.display()
            ))),
        },
        Some(1) => {
            let machine_id = parsed.machine_id.as_deref().ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "host identity '{}' is incomplete: missing or blank machine_id",
                    path.display()
                ))
            })?;
            validate_machine_id(machine_id).map_err(|error| {
                OrbitError::InvalidInput(format!(
                    "host identity '{}' has invalid machine_id: {error}",
                    path.display()
                ))
            })?;
            let host_id = non_blank(&parsed.host_id).ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "host identity '{}' is incomplete: missing or blank host_id",
                    path.display()
                ))
            })?;
            Ok(HostIdentityState::Legacy {
                host_id,
                machine_id: Some(machine_id.to_string()),
            })
        }
        Some(version) if version == HOST_IDENTITY_SCHEMA_VERSION => {
            let machine_id = parsed.machine_id.as_deref().ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "host identity '{}' is incomplete: missing or blank machine_id",
                    path.display()
                ))
            })?;
            validate_machine_id(machine_id).map_err(|error| {
                OrbitError::InvalidInput(format!(
                    "host identity '{}' has invalid machine_id: {error}",
                    path.display()
                ))
            })?;
            let host_id = non_blank(&parsed.host_id).ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "host identity '{}' is incomplete: missing or blank host_id",
                    path.display()
                ))
            })?;
            let task_prefix = parsed.task_prefix.as_deref().ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "host identity '{}' is incomplete: missing or blank task_prefix",
                    path.display()
                ))
            })?;
            let task_prefix = validate_stored_task_prefix(task_prefix).map_err(|error| {
                OrbitError::InvalidInput(format!(
                    "host identity '{}' has invalid task_prefix: {error}",
                    path.display()
                ))
            })?;
            Ok(HostIdentityState::Present(HostIdentity {
                schema_version: version,
                machine_id: machine_id.to_string(),
                host_id,
                task_prefix,
            }))
        }
        Some(version) if version > HOST_IDENTITY_SCHEMA_VERSION => {
            Err(OrbitError::InvalidInput(format!(
                "host identity '{}' has unsupported schema_version {version}; this build \
                 supports up to {HOST_IDENTITY_SCHEMA_VERSION}. Upgrade Orbit; the file is \
                 left unchanged",
                path.display()
            )))
        }
        Some(version) => Err(OrbitError::InvalidInput(format!(
            "host identity '{}' has invalid schema_version {version}",
            path.display()
        ))),
    }
}

fn validated_optional_machine_id(
    path: &Path,
    machine_id: Option<&str>,
) -> Result<Option<String>, OrbitError> {
    let Some(machine_id) = machine_id else {
        return Ok(None);
    };
    validate_machine_id(machine_id).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "host identity '{}' has invalid machine_id: {error}",
            path.display()
        ))
    })?;
    Ok(Some(machine_id.to_string()))
}

/// Strictly load a complete, current-schema identity. Absent and legacy files
/// are errors here (they resolve through `orbit init`); malformed / future
/// files propagate from [`inspect_host_identity`]. Never falls back to the OS
/// hostname.
pub fn load_host_identity(global_root: &Path) -> Result<HostIdentity, OrbitError> {
    match inspect_host_identity(global_root)? {
        HostIdentityState::Present(identity) => Ok(identity),
        HostIdentityState::Legacy { .. } => Err(OrbitError::InvalidInput(format!(
            "host identity '{}' is a legacy pre-migration file; run `orbit init` to migrate it",
            host_toml_path(global_root).display()
        ))),
        HostIdentityState::Absent => Err(OrbitError::InvalidInput(format!(
            "no host identity at '{}'; run `orbit init` to create one",
            host_toml_path(global_root).display()
        ))),
    }
}

/// Ensure a complete, current-schema identity exists, creating or migrating as
/// needed and returning what happened. `new` supplies the operator's host name
/// and task prefix and is invoked **only** when the identity is absent, so
/// callers can defer prompting until a fresh create is actually required.
///
/// Idempotent: a present identity is returned `Unchanged` with no write. A
/// malformed or future-schema file is never overwritten — the error propagates.
///
/// Concurrent callers on the same root are serialised with an exclusive lock
/// on `host.toml`. Inspect runs after the lock is held, so a racing second
/// create observes `Present` and returns `Unchanged` instead of minting a
/// second `machine_id`.
pub fn ensure_host_identity(
    global_root: &Path,
    new: impl FnOnce() -> Result<NewHostIdentity, OrbitError>,
) -> Result<HostIdentityOutcome, OrbitError> {
    with_exclusive_file_lock(&host_toml_path(global_root), "host identity", move || {
        match inspect_host_identity(global_root)? {
            HostIdentityState::Present(identity) => Ok(HostIdentityOutcome::Unchanged(identity)),
            HostIdentityState::Legacy {
                host_id,
                machine_id,
            } => {
                // Migrate atomically: preserve an existing schema-v1 machine id,
                // generate one only for the oldest host-id-only format, and retain
                // the historical ORB namespace. Rollback leaves the last valid
                // file readable.
                let identity = HostIdentity {
                    schema_version: HOST_IDENTITY_SCHEMA_VERSION,
                    machine_id: machine_id.unwrap_or_else(generate_machine_id),
                    host_id,
                    task_prefix: LEGACY_TASK_PREFIX.to_string(),
                };
                write_host_identity(global_root, &identity)?;
                Ok(HostIdentityOutcome::Migrated(identity))
            }
            HostIdentityState::Absent => {
                let NewHostIdentity {
                    host_id,
                    task_prefix,
                } = new()?;
                let host_id = host_id.trim().to_string();
                if host_id.is_empty() {
                    return Err(OrbitError::InvalidInput(
                        "host name must not be empty".to_string(),
                    ));
                }
                let task_prefix = validate_new_task_prefix(&task_prefix)?;
                let identity = HostIdentity {
                    schema_version: HOST_IDENTITY_SCHEMA_VERSION,
                    machine_id: generate_machine_id(),
                    host_id,
                    task_prefix,
                };
                write_host_identity(global_root, &identity)?;
                Ok(HostIdentityOutcome::Created(identity))
            }
        }
    })
}

/// Atomically (re)write `host.toml`. The staged-rename write never leaves a
/// partially overwritten file, so a crash preserves the last valid identity.
fn write_host_identity(global_root: &Path, identity: &HostIdentity) -> Result<PathBuf, OrbitError> {
    let text = stage_host_identity_toml(identity)?;
    write_host_identity_text(global_root, &text)
}

/// Atomically write already-staged `host.toml` bytes through the crash-safe
/// atomic-write seam.
fn write_host_identity_text(global_root: &Path, text: &str) -> Result<PathBuf, OrbitError> {
    let path = host_toml_path(global_root);
    atomic_write_text(&path, text).map_err(|error| {
        OrbitError::Io(format!("failed to write '{}': {error}", path.display()))
    })?;
    Ok(path)
}

/// Render `host.toml` and reparse the staged bytes before any write, confirming
/// every field round-trips to the intended identity. A quote, backslash, or
/// control character that would corrupt the file is caught here — before
/// mutation — rather than producing an unreadable on-disk identity.
fn stage_host_identity_toml(identity: &HostIdentity) -> Result<String, OrbitError> {
    let rendered = identity.to_toml();
    let parsed: RawHostToml = toml::from_str(&rendered).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "staged host identity render is not valid TOML: {error}"
        ))
    })?;
    let round_trips = parsed.schema_version == Some(identity.schema_version)
        && non_blank(&parsed.machine_id).as_deref() == Some(identity.machine_id.as_str())
        && non_blank(&parsed.host_id).as_deref() == Some(identity.host_id.as_str())
        && non_blank(&parsed.task_prefix).as_deref() == Some(identity.task_prefix.as_str());
    if !round_trips {
        return Err(OrbitError::InvalidInput(
            "staged host identity render does not round-trip to the intended identity; \
             refusing to write a corrupt host.toml"
                .to_string(),
        ));
    }
    Ok(rendered)
}

/// Rename the current machine's local `host.toml` in place, preserving
/// `machine_id` and `task_prefix`. The identity must already be `Present`; a
/// legacy or absent file is a hard error (there is no local identity to rename).
/// The new render is staged and reparsed before the atomic write, so quotes,
/// backslashes, and control characters either round-trip safely or fail before
/// mutation and the last valid file is preserved. Renaming another machine must
/// never call this — it only ever touches the local file.
pub fn rename_current_host_identity(
    global_root: &Path,
    new_host_id: &str,
) -> Result<HostIdentity, OrbitError> {
    rename_current_host_identity_with_writer(global_root, new_host_id, |path, staged| {
        atomic_write_text(path, staged)
    })
}

pub(crate) fn rename_current_host_identity_with_writer<W>(
    global_root: &Path,
    new_host_id: &str,
    writer: W,
) -> Result<HostIdentity, OrbitError>
where
    W: FnOnce(&Path, &str) -> std::io::Result<()>,
{
    let current = load_host_identity(global_root)?;
    let new_host_id = new_host_id.trim();
    if new_host_id.is_empty() {
        return Err(OrbitError::InvalidInput(
            "host name must not be empty".to_string(),
        ));
    }
    let candidate = HostIdentity {
        schema_version: current.schema_version,
        machine_id: current.machine_id.clone(),
        host_id: new_host_id.to_string(),
        task_prefix: current.task_prefix.clone(),
    };
    let staged = stage_host_identity_toml(&candidate)?;
    let path = host_toml_path(global_root);
    match writer(&path, &staged) {
        Ok(()) => Ok(candidate),
        Err(error) => match load_host_identity(global_root) {
            Ok(observed) if observed == candidate => Err(OrbitError::Io(format!(
                "host identity write reported an error ({error}), but the complete renamed \
                 host.toml is now readable; its durability is uncertain"
            ))),
            Ok(observed) if observed == current => Err(OrbitError::Io(format!(
                "host identity write failed ({error}); the previous host.toml is preserved"
            ))),
            Ok(observed) => Err(OrbitError::Io(format!(
                "host identity write failed ({error}); reopening found an unexpected complete \
                 identity for machine '{}' named '{}', so the local outcome is uncertain",
                observed.machine_id, observed.host_id
            ))),
            Err(reopen) => Err(OrbitError::Io(format!(
                "host identity write failed ({error}); reopening host.toml to classify \
                 preservation also failed: {reopen}"
            ))),
        },
    }
}

/// Best-effort OS hostname, used as the interactive default host name at init.
/// `None` when the hostname is unavailable or empty.
pub fn os_hostname() -> Option<String> {
    hostname::get()
        .ok()
        .map(|name| name.to_string_lossy().trim().to_string())
        .filter(|name| !name.is_empty())
}

static MACHINE_ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Generate an opaque, stable `hm_<hex>` machine id. Called exactly once per
/// machine (at create / migrate); the persisted value is never regenerated.
fn generate_machine_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let pid = u64::from(std::process::id());
    let seq = MACHINE_ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let seed =
        nanos ^ pid.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ seq.wrapping_mul(0xD1B5_4A32_D192_ED03);
    format!("{MACHINE_ID_PREFIX}{:016x}", splitmix64(seed))
}

/// SplitMix64 finalizer — folds a seed into a well-distributed 64-bit value.
fn splitmix64(seed: u64) -> u64 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
