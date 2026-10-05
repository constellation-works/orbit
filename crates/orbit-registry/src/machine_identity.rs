//! Machine identity for this Orbit installation [ORB-10247, ORB-10721, ORB-12725].
//!
//! The global `~/.orbit/config.toml` carries a `[machine]` table with a
//! stable, generated `id`, an operator-chosen `name`, and an immutable task
//! namespace. It lives beside this user's crews and delivery settings because
//! that is what it is: a per-user, per-machine fact, in one file, shown by one
//! command. First-time creation lives in the global `orbit init` flow.
//!
//! The identity is read from the admitted config snapshot rather than parsed
//! here, so `orbit config show`, `orbit config get machine.id`, and every
//! runtime consumer resolve exactly the same value through exactly the same
//! validation.
//!
//! Loading is strict: a partial or invalid `[machine]` table is a hard error
//! with an actionable message — there is no silent fallback to the OS hostname
//! and no regeneration. A `machine.task_prefix` that contradicts the ids the
//! local task store has already minted fails closed there, at the allocator.
//!
//! One release of compatibility: a pre-ORB-12725 `~/.orbit/host.toml` is
//! folded into `[machine]` on first load and the file is removed.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use orbit_common::OrbitError;
use orbit_common::fs::io::with_exclusive_file_lock;
use orbit_common::fs::open_read_only_no_follow;
use orbit_config::{ConfigScope, ConfigStore, load_machine_settings};
use orbit_types::identity::{
    LEGACY_TASK_PREFIX, MACHINE_ID_PREFIX, validate_machine_id, validate_machine_name,
    validate_new_task_prefix,
};
use serde::Deserialize;

/// The global `config.toml` this machine's `[machine]` table lives in.
pub const CONFIG_TOML_FILE: &str = "config.toml";
/// Pre-ORB-12725 identity file, read once and removed by [`migrate_host_toml`].
pub const LEGACY_HOST_TOML_FILE: &str = "host.toml";

/// This machine's identity, as admitted from the global `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineIdentity {
    /// Opaque, generated-once, never-reused stable identity (`hm_<hex>`).
    pub id: String,
    /// Operator-chosen, renameable display name.
    pub name: String,
    /// Immutable namespace for task ids minted by this machine.
    pub task_prefix: String,
}

/// The two actionable states of `[machine]`. A partial or invalid table is
/// returned as `Err` by [`inspect_machine_identity`], never as a variant here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineIdentityState {
    /// A complete, validated identity.
    Present(MachineIdentity),
    /// No `[machine]` table exists yet.
    Absent,
}

impl MachineIdentityState {
    /// The display name this state resolves to, if any.
    pub fn name(&self) -> Option<&str> {
        match self {
            MachineIdentityState::Present(identity) => Some(&identity.name),
            MachineIdentityState::Absent => None,
        }
    }

    /// The stable machine id this state resolves to, if any.
    pub fn id(&self) -> Option<&str> {
        match self {
            MachineIdentityState::Present(identity) => Some(&identity.id),
            MachineIdentityState::Absent => None,
        }
    }
}

/// Outcome of [`ensure_machine_identity`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineIdentityOutcome {
    /// A fresh identity was created from operator input.
    Created(MachineIdentity),
    /// A pre-ORB-12725 `host.toml` was folded into `[machine]`.
    Migrated(MachineIdentity),
    /// A complete identity already existed; nothing was written.
    Unchanged(MachineIdentity),
}

impl MachineIdentityOutcome {
    /// The resulting identity, regardless of how it was reached.
    pub fn identity(&self) -> &MachineIdentity {
        match self {
            MachineIdentityOutcome::Created(identity)
            | MachineIdentityOutcome::Migrated(identity)
            | MachineIdentityOutcome::Unchanged(identity) => identity,
        }
    }
}

/// Operator-supplied fields for a first-time identity creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMachineIdentity {
    /// Operator-chosen display name.
    pub name: String,
    /// Operator-chosen task namespace.
    pub task_prefix: String,
}

fn config_path(global_root: &Path) -> PathBuf {
    global_root.join(CONFIG_TOML_FILE)
}

fn host_toml_path(global_root: &Path) -> PathBuf {
    global_root.join(LEGACY_HOST_TOML_FILE)
}

/// Classify this machine's identity without changing it.
///
/// Folds a legacy `host.toml` into `[machine]` first (see
/// [`migrate_host_toml`]), then reads the admitted global config snapshot. A
/// partial or invalid table propagates as the loader's own actionable error.
///
/// When the migration cannot persist — a read-only root, a sandboxed reader —
/// the legacy identity is still returned from memory. Resolving who this
/// machine is is a read; it must not depend on being able to write.
pub fn inspect_machine_identity(global_root: &Path) -> Result<MachineIdentityState, OrbitError> {
    let legacy = migrate_host_toml(global_root)?;
    // Admission already refuses a partial table, so all three arrive together.
    match load_machine_settings(global_root)?.complete() {
        Some((id, name, task_prefix)) => Ok(MachineIdentityState::Present(MachineIdentity {
            id,
            name,
            task_prefix,
        })),
        None => Ok(match legacy {
            Some(identity) => MachineIdentityState::Present(identity),
            None => MachineIdentityState::Absent,
        }),
    }
}

/// Strictly load a complete identity. An absent `[machine]` table is an error
/// here (it resolves through `orbit init`); an invalid one propagates from
/// [`inspect_machine_identity`]. Never falls back to the OS hostname.
pub fn load_machine_identity(global_root: &Path) -> Result<MachineIdentity, OrbitError> {
    match inspect_machine_identity(global_root)? {
        MachineIdentityState::Present(identity) => Ok(identity),
        MachineIdentityState::Absent => Err(OrbitError::InvalidInput(format!(
            "no [machine] identity in '{}'; run `orbit init` to create one",
            config_path(global_root).display()
        ))),
    }
}

/// Ensure a complete identity exists, creating it if needed and reporting what
/// happened. `new` supplies the operator's machine name and task prefix and is
/// invoked **only** when the identity is absent, so callers can defer
/// prompting until a fresh create is actually required.
///
/// Idempotent: a present identity is returned `Unchanged` with no write, and a
/// migrated `host.toml` is reported as `Migrated`.
///
/// Concurrent callers on the same root are serialised with an exclusive lock on
/// the global `config.toml`. Classification runs after the lock is held, so a
/// racing second create observes the identity and returns `Unchanged` instead
/// of minting a second `machine.id`.
pub fn ensure_machine_identity(
    global_root: &Path,
    new: impl FnOnce() -> Result<NewMachineIdentity, OrbitError>,
) -> Result<MachineIdentityOutcome, OrbitError> {
    with_exclusive_file_lock(&config_path(global_root), "machine identity", move || {
        let migrated = migrate_host_toml(global_root)?.is_some();
        if let MachineIdentityState::Present(identity) = inspect_machine_identity(global_root)? {
            return Ok(if migrated {
                MachineIdentityOutcome::Migrated(identity)
            } else {
                MachineIdentityOutcome::Unchanged(identity)
            });
        }
        let NewMachineIdentity { name, task_prefix } = new()?;
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(OrbitError::InvalidInput(
                "machine name must not be empty".to_string(),
            ));
        }
        validate_machine_name(&name)?;
        let task_prefix = validate_new_task_prefix(&task_prefix)?;
        let identity = MachineIdentity {
            id: generate_machine_id(),
            name,
            task_prefix,
        };
        write_machine_identity(global_root, &identity)?;
        Ok(MachineIdentityOutcome::Created(identity))
    })
}

/// Record `identity` in the global `config.toml`, preserving every other key
/// and every hand-written comment. The store reparses the staged document, so
/// a value that would not round-trip fails before mutation.
fn write_machine_identity(
    global_root: &Path,
    identity: &MachineIdentity,
) -> Result<(), OrbitError> {
    let mut store = ConfigStore::open(ConfigScope::Global, config_path(global_root))?;
    store.set_machine_identity(&identity.id, &identity.name, &identity.task_prefix)?;
    store.save()
}

/// Fold a pre-ORB-12725 `~/.orbit/host.toml` into `[machine]`, then remove it.
///
/// Returns whether a migration was performed. Absent `host.toml` is the
/// steady state and costs one metadata call. A `host.toml` that disagrees with
/// an existing `[machine]` table is refused naming both files: the two are
/// different answers to "who is this machine", and picking one silently would
/// either orphan the ids minted under the other or rewrite an identity the
/// operator never asked to change.
pub fn migrate_host_toml(global_root: &Path) -> Result<Option<MachineIdentity>, OrbitError> {
    let Some(legacy) = read_host_toml(global_root)? else {
        return Ok(None);
    };
    let config_path = config_path(global_root);
    let host_path = host_toml_path(global_root);

    if let Some((id, name, task_prefix)) = load_machine_settings(global_root)?.complete() {
        let current = MachineIdentity {
            id,
            name,
            task_prefix,
        };
        if current == legacy {
            remove_host_toml(global_root);
            return Ok(None);
        }
        return Err(OrbitError::InvalidInput(format!(
            "'{}' and '{}' name different machines: host.toml says id={}, name=\"{}\", \
             task_prefix={}; config.toml says id={}, name=\"{}\", task_prefix={}. \
             Reconcile them by hand — delete the stale file — before running Orbit again",
            host_path.display(),
            config_path.display(),
            legacy.id,
            legacy.name,
            legacy.task_prefix,
            current.id,
            current.name,
            current.task_prefix,
        )));
    }

    // The write is the optimization, not the answer: a read-only or sandboxed
    // root still resolves the legacy identity from memory and tries again on a
    // later writable open.
    match write_machine_identity(global_root, &legacy) {
        Ok(()) => {
            remove_host_toml(global_root);
            tracing::info!(
                machine_id = %legacy.id,
                machine_name = %legacy.name,
                config = %config_path.display(),
                "migrated host.toml into the [machine] table and removed it"
            );
        }
        Err(error) => tracing::warn!(
            %error,
            config = %config_path.display(),
            "could not fold host.toml into the [machine] table; \
             using the legacy identity for this process"
        ),
    }
    Ok(Some(legacy))
}

/// Best-effort removal of the migrated file. A root Orbit can read but not
/// write keeps its `host.toml`; the next writable open retires it.
fn remove_host_toml(global_root: &Path) {
    let path = match validated_host_toml_path(global_root) {
        Ok(Some(path)) => path,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(
                %error,
                root = %global_root.display(),
                "could not validate host.toml path for removal"
            );
            return;
        }
    };
    if let Err(error) = std::fs::remove_file(&path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(
            %error,
            path = %path.display(),
            "could not remove the migrated host.toml"
        );
    }
}

/// CodeQL `rust/path-injection` treats `Path::starts_with` as a SafeAccessCheck
/// on the receiver. Call this after reconstructing `host.toml` so later
/// filesystem sinks only see a prefix-checked value.
fn host_toml_path_is_contained(path: &Path, parent: &Path) -> bool {
    path.starts_with(parent)
}

/// Reconstruct the legacy identity file under a validated existing global root
/// before any remove sink.
fn validated_host_toml_path(global_root: &Path) -> Result<Option<PathBuf>, OrbitError> {
    let Some(canonical_root) = validated_existing_global_root(global_root)? else {
        return Ok(None);
    };
    let candidate = canonical_root.join(LEGACY_HOST_TOML_FILE);
    if !host_toml_path_is_contained(&candidate, &canonical_root) {
        return Err(OrbitError::InvalidInput(format!(
            "legacy host identity path escapes its parent: {}",
            candidate.display()
        )));
    }
    Ok(Some(candidate))
}

#[derive(Debug, Deserialize)]
struct RawHostToml {
    machine_id: Option<String>,
    /// The legacy file's own key for what is now `machine.name`.
    #[serde(rename = "host_id")]
    machine_name: Option<String>,
    task_prefix: Option<String>,
}

/// Read and validate a legacy `host.toml`, if one exists.
///
/// Every shipped on-disk shape is accepted: the oldest host-id-only file has
/// no `machine_id` and receives one here, and a v1 file has no `task_prefix`
/// and keeps the historical `ORB` namespace, exactly as the pre-ORB-12725
/// in-place migration did.
fn read_host_toml(global_root: &Path) -> Result<Option<MachineIdentity>, OrbitError> {
    let Some((path, mut file)) = open_existing_host_toml(global_root)? else {
        return Ok(None);
    };
    let mut raw_text = String::new();
    file.read_to_string(&mut raw_text)
        .map_err(|error| OrbitError::Io(format!("failed to read '{}': {error}", path.display())))?;
    let parsed: RawHostToml = toml::from_str(&raw_text).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "invalid legacy host identity '{}': {error}",
            path.display()
        ))
    })?;

    let name = non_blank(&parsed.machine_name).ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "legacy host identity '{}' is incomplete: missing or blank host_id; \
             delete it and run `orbit init`",
            path.display()
        ))
    })?;
    validate_machine_name(&name).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "legacy host identity '{}' has invalid host_id: {error}",
            path.display()
        ))
    })?;
    let id = match non_blank(&parsed.machine_id) {
        Some(machine_id) => {
            validate_machine_id(&machine_id).map_err(|error| {
                OrbitError::InvalidInput(format!(
                    "legacy host identity '{}' has invalid machine_id: {error}",
                    path.display()
                ))
            })?;
            machine_id
        }
        None => generate_machine_id(),
    };
    let task_prefix = match non_blank(&parsed.task_prefix) {
        Some(task_prefix) => task_prefix,
        None => LEGACY_TASK_PREFIX.to_string(),
    };
    Ok(Some(MachineIdentity {
        id,
        name,
        task_prefix,
    }))
}

/// Resolve an existing global root to the directory selected by the caller.
///
/// Runtime overrides and configured aliases are supported: an existing
/// symlinked root resolves to its canonical target. Returning the validated
/// directory before deriving the fixed filename keeps path validation ahead of
/// every metadata and open sink for `host.toml`.
fn validated_existing_global_root(global_root: &Path) -> Result<Option<PathBuf>, OrbitError> {
    match global_root.canonicalize() {
        Ok(path) => Ok(Some(path)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(OrbitError::Io(format!(
            "failed to canonicalize machine identity directory '{}': {error}",
            global_root.display()
        ))),
    }
}

/// Open the fixed legacy identity file beneath the validated root without
/// following a swapped final symlink.
///
/// Unix uses `O_NOFOLLOW | O_NONBLOCK`; Windows opens the reparse point itself.
/// The descriptor is checked for a regular file before any bytes are read.
/// Platforms without either primitive still perform both pathname and
/// descriptor type checks, but cannot close a final-component check/open race.
fn open_existing_host_toml(global_root: &Path) -> Result<Option<(PathBuf, File)>, OrbitError> {
    let Some(canonical_root) = validated_existing_global_root(global_root)? else {
        return Ok(None);
    };
    let path = canonical_root.join(LEGACY_HOST_TOML_FILE);

    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(OrbitError::InvalidInput(format!(
                "legacy host identity path must be a regular {LEGACY_HOST_TOML_FILE} file inside \
                 '{}': {}",
                global_root.display(),
                path.display()
            )))
        }
        Ok(_) => {
            let file = open_read_only_no_follow(&path).map_err(|error| {
                OrbitError::Io(format!("failed to open '{}': {error}", path.display()))
            })?;
            let metadata = file.metadata().map_err(|error| {
                OrbitError::Io(format!("failed to inspect '{}': {error}", path.display()))
            })?;
            if !metadata.is_file() {
                return Err(OrbitError::InvalidInput(format!(
                    "legacy host identity path must open as a regular {LEGACY_HOST_TOML_FILE} \
                     file inside '{}': {}",
                    global_root.display(),
                    path.display()
                )));
            }
            Ok(Some((path, file)))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(OrbitError::Io(format!(
            "failed to inspect legacy host identity '{}': {error}",
            path.display()
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

/// Best-effort OS hostname, used as the interactive default machine name at
/// init. `None` when the hostname is unavailable or empty.
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
