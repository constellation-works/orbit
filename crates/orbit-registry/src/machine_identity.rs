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

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use orbit_common::OrbitError;
use orbit_common::fs::io::with_exclusive_file_lock;
use orbit_config::{ConfigScope, ConfigStore, load_machine_settings};
use orbit_types::identity::{MACHINE_ID_PREFIX, validate_machine_name, validate_new_task_prefix};

/// The global `config.toml` this machine's `[machine]` table lives in.
pub const CONFIG_TOML_FILE: &str = "config.toml";
/// Pre-ORB-12725 identity file, kept only to name it in the refusal when it
/// is the sole identity source.
const LEGACY_HOST_TOML_FILE: &str = "host.toml";

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
    /// A complete identity already existed; nothing was written.
    Unchanged(MachineIdentity),
}

impl MachineIdentityOutcome {
    /// The resulting identity, regardless of how it was reached.
    pub fn identity(&self) -> &MachineIdentity {
        match self {
            MachineIdentityOutcome::Created(identity)
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

/// Resolve an existing global root to its canonical directory.
///
/// Trusted aliases remain supported: a symlinked root resolves to its
/// canonical target, exactly as joining through it did before. `None` means
/// the root does not exist, so a legacy file beneath it cannot exist either.
fn validated_existing_global_root(global_root: &Path) -> Result<Option<PathBuf>, OrbitError> {
    match global_root.canonicalize() {
        Ok(path) => Ok(Some(path)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(OrbitError::Io(format!(
            "failed to canonicalize global root '{}': {error}",
            global_root.display()
        ))),
    }
}

/// CodeQL `rust/path-injection` treats `Path::starts_with` as a
/// SafeAccessCheck on the receiver. Call this after reconstructing the
/// legacy path so the metadata probe only sees a prefix-checked value.
fn host_toml_path_is_contained(path: &Path, parent: &Path) -> bool {
    path.starts_with(parent)
}

/// The legacy `host.toml` path under a validated global root: the fixed
/// leaf name joined to the canonical root and prefix-checked against it.
/// `None` means the root itself is absent.
fn validated_host_toml_path(global_root: &Path) -> Result<Option<PathBuf>, OrbitError> {
    let Some(root) = validated_existing_global_root(global_root)? else {
        return Ok(None);
    };
    let candidate = root.join(LEGACY_HOST_TOML_FILE);
    if !host_toml_path_is_contained(&candidate, &root) {
        return Err(OrbitError::InvalidInput(format!(
            "legacy host identity path escapes its global root '{}'",
            global_root.display()
        )));
    }
    Ok(Some(candidate))
}

/// Whether a legacy `host.toml` exists under `global_root`, probed with a
/// no-follow metadata call only; its contents are never parsed. The probe
/// sees only the validated, reconstructed path, never a caller-supplied
/// path joined directly.
fn legacy_host_toml_exists(global_root: &Path) -> Result<bool, OrbitError> {
    let Some(path) = validated_host_toml_path(global_root)? else {
        return Ok(false);
    };
    match std::fs::symlink_metadata(&path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(OrbitError::Io(format!(
            "failed to inspect legacy host identity '{}': {error}",
            path.display()
        ))),
    }
}

/// Classify this machine's identity without changing it.
///
/// Reads the admitted global config snapshot's `[machine]` table only. A
/// partial or invalid table propagates as the loader's own actionable error.
/// When `[machine]` is absent and a legacy `host.toml` exists, this refuses
/// naming both files with the manual repair, rather than letting a later
/// `orbit init` mint a new identity and orphan ids already minted under the
/// legacy one. When `[machine]` is present, a leftover `host.toml` is ignored.
pub fn inspect_machine_identity(global_root: &Path) -> Result<MachineIdentityState, OrbitError> {
    // Admission already refuses a partial table, so all three arrive together.
    match load_machine_settings(global_root)?.complete() {
        Some((id, name, task_prefix)) => Ok(MachineIdentityState::Present(MachineIdentity {
            id,
            name,
            task_prefix,
        })),
        None => {
            if legacy_host_toml_exists(global_root)? {
                let host_path = global_root.join(LEGACY_HOST_TOML_FILE);
                let config = config_path(global_root);
                return Err(OrbitError::InvalidInput(format!(
                    "'{}' holds this machine's identity but '{}' has no [machine] table; \
                     Orbit no longer migrates host.toml automatically. Copy machine_id as \
                     [machine] id, host_id as machine.name, and task_prefix as \
                     machine.task_prefix from '{}' into the [machine] table in '{}' by hand, \
                     then delete '{}'",
                    host_path.display(),
                    config.display(),
                    host_path.display(),
                    config.display(),
                    host_path.display(),
                )));
            }
            Ok(MachineIdentityState::Absent)
        }
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
/// Idempotent: a present identity is returned `Unchanged` with no write.
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
        if let MachineIdentityState::Present(identity) = inspect_machine_identity(global_root)? {
            return Ok(MachineIdentityOutcome::Unchanged(identity));
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

/// Record `identity` in the global `config.toml`, with the machine defaults a
/// new machine starts from (an operator-set value wins), preserving every other key
/// and every hand-written comment. The store reparses the staged document, so
/// a value that would not round-trip fails before mutation. The read and save
/// run under the config write lock, so a concurrent `orbit config` write is
/// not erased.
fn write_machine_identity(
    global_root: &Path,
    identity: &MachineIdentity,
) -> Result<(), OrbitError> {
    let path = config_path(global_root);
    ConfigStore::with_write_lock(&path, || {
        let mut store = ConfigStore::open(ConfigScope::Global, path.clone())?;
        store.set_machine_identity(&identity.id, &identity.name, &identity.task_prefix)?;
        orbit_config::seed_new_machine_defaults(&mut store)?;
        store.save()
    })
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
/// machine (at create); the persisted value is never regenerated.
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
