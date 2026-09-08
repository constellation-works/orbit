//! Whether a global Orbit root already carries this binary's managed defaults.
//!
//! Reconciling the shipped defaults renders, hashes, and re-reads every managed
//! skill, activity, job, executor, and policy. That work only changes anything
//! when the embedded asset set — or the root it renders into — differs from the
//! last reconciliation, yet implicit bootstrap ran it on every runtime open.
//!
//! Each completed global-scope reconciliation therefore records a stamp naming
//! the asset set it installed. A later open compares that stamp against the
//! running binary and skips the whole pass when they agree, so a warm open
//! performs no managed-asset reads or digests at all.
//!
//! The stamp answers exactly one question: *did this binary already reconcile
//! this root?* It deliberately says nothing about what the root looks like now.
//! Repairing a root whose managed files were edited or deleted by hand belongs
//! to the explicit paths — `orbit init` and `orbit workspace sync` — which
//! never consult the stamp. `orbit doctor` also never consults the stamp: it
//! compares each previously reconciled managed catalog against the embedded
//! default set and reports a missing shipped default instead of calling the
//! root healthy. Restoration remains `orbit init` / `orbit workspace sync`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::application::executor::DEFAULT_EXECUTOR_FILES;
use crate::application::job::DEFAULT_JOB_FILES;
use crate::application::skill::DEFAULT_SKILL_FILES;
use crate::bootstrap::policy::DEFAULT_POLICY_FILES;
use crate::runtime::assets::DEFAULT_ACTIVITY_FILES;

const GLOBAL_DEFAULTS_STAMP_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GlobalDefaultsStamp {
    schema_version: u32,
    defaults_digest: String,
}

/// Whether `global_root` was already reconciled against the defaults this
/// binary embeds. An absent, unreadable, or unrecognized stamp reads as stale,
/// so an unusable stamp costs one reconciliation rather than correctness.
pub(crate) fn global_defaults_are_current(global_root: &Path) -> bool {
    let Ok(raw) = fs::read_to_string(stamp_path(global_root)) else {
        return false;
    };
    let Ok(stamp) = serde_json::from_str::<GlobalDefaultsStamp>(&raw) else {
        return false;
    };

    stamp.schema_version == GLOBAL_DEFAULTS_STAMP_SCHEMA_VERSION
        && stamp.defaults_digest == installed_defaults_digest(global_root)
}

/// Record that a global-scope reconciliation just installed this binary's
/// defaults into `global_root`. Callers invoke this only after every default
/// landed, so a partial or failed seed leaves the next open reconciling.
pub(crate) fn record_global_defaults_reconciled(global_root: &Path) -> Result<(), OrbitError> {
    let stamp = GlobalDefaultsStamp {
        schema_version: GLOBAL_DEFAULTS_STAMP_SCHEMA_VERSION,
        defaults_digest: installed_defaults_digest(global_root),
    };
    let mut encoded = serde_json::to_string_pretty(&stamp)
        .map_err(|error| OrbitError::Store(format!("serialize global defaults stamp: {error}")))?;
    encoded.push('\n');

    let path = stamp_path(global_root);
    atomic_write_text(&path, &encoded).map_err(|error| {
        OrbitError::Io(format!(
            "write global defaults stamp '{}': {error}",
            path.display()
        ))
    })
}

/// Where a global root records its stamp: beside the resource catalogs it
/// describes, and never in `state/`, which the layout reserves for workspace
/// runtime state. Exposed so fixtures can model a root left by another release
/// without duplicating the path.
pub(crate) fn stamp_path(global_root: &Path) -> PathBuf {
    global_root
        .join("resources")
        .join(".orbit-global-defaults.json")
}

/// Identity of the defaults a global-scope bootstrap installs into one root.
///
/// The embedded set is the bulk of it, but rendering is root- and
/// platform-dependent — skill content carries the root path, and the shipped
/// executors are seeded per host OS — so both join the digest. Two Orbit
/// binaries sharing one root simply disagree and each reconciles once.
fn installed_defaults_digest(global_root: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(embedded_defaults_digest());
    hasher.update([0]);
    hasher.update(global_root.to_string_lossy().as_bytes());
    hasher.update([0]);
    hasher.update(std::env::consts::OS.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Digest of every embedded default a global-scope bootstrap seeds. The inputs
/// are compile-time constants, so this is computed once per process.
fn embedded_defaults_digest() -> &'static str {
    static DIGEST: OnceLock<String> = OnceLock::new();

    DIGEST.get_or_init(|| {
        let mut hasher = Sha256::new();
        for (asset_kind, assets) in [
            ("skill", &DEFAULT_SKILL_FILES[..]),
            ("activity", DEFAULT_ACTIVITY_FILES),
            ("job", DEFAULT_JOB_FILES),
            ("executor", DEFAULT_EXECUTOR_FILES),
            ("policy", DEFAULT_POLICY_FILES),
        ] {
            hasher.update(asset_kind.as_bytes());
            hasher.update([0]);
            for (name, content) in assets {
                hasher.update(name.as_bytes());
                hasher.update([0]);
                hasher.update(content.as_bytes());
                hasher.update([0]);
            }
        }
        format!("{:x}", hasher.finalize())
    })
}
