//! The authorization witness that makes a plugin's recorded grants
//! tamper-evident [ORB-12778].
//!
//! `orbit plugin enable --grant …` is the only source of a plugin's authority
//! (design `docs/design/plugins/1_scope.md` §4.1), but the grant set it
//! records lives in the `plugins` row of `orbit.db`, and a backend holding
//! `orbit_tools` can write that file — ORB-12777 confined the sandbox to named
//! paths and the store is necessarily one of them, because the child cannot
//! run `orbit tool run` without it. A plugin could therefore `UPDATE plugins
//! SET grants_json='[…,"unsandboxed"]'` and be registered unconfined at the
//! next load.
//!
//! So the authorizing command also writes an integrity value over the grant
//! set to `<global_root>/plugins/.grants/<ns>.json`, and the loader refuses a
//! row whose grants do not match it. The two halves sit on opposite sides of a
//! boundary the kernel already enforces: `plugins/` is read-only to a confined
//! backend, `orbit.db` is not.
//!
//! The value is a plain digest, not a MAC. A keyed value would need a secret
//! the child cannot read, and a child holding `orbit_tools` reads the whole
//! global root — it could read any key it could then forge with. What bounds
//! the attacker here is the write boundary, not a secret.
//!
//! # What the witness binds, and what binds the rest of the row
//!
//! The digest covers exactly three row fields: `name`, `enabled` and
//! `grants_json`. Every other field of the `plugins` row is as writable as
//! those, so each is held by something else, and the reader should not assume
//! the witness covers it:
//!
//! - `install_path` — **not** in the witness. It is bound structurally by
//!   [`verify_install_path`]: the loader refuses a row whose path does not
//!   resolve beneath `<global_root>/plugins/<name>/`, the directory the
//!   confined backend cannot write, before it reads anything from that path
//!   [ORB-12785]. Without this, a row could keep its authorized grant names
//!   and point them at a tree the backend wrote under one of its own write
//!   roots, and the witness would still match.
//! - `manifest_digest` — **not** in the witness, deliberately: grants survive
//!   `orbit plugin add` of a newer version, and binding the digest would refuse
//!   every upgrade. It is only meaningful *because* `install_path` is bound:
//!   the loader compares it against the bytes at a path the attacker cannot
//!   populate, so agreeing with it proves the tree is the one `orbit plugin
//!   add` copied there (§4.1).
//! - `version` — **not** in the witness, for the same upgrade reason; it names
//!   the install directory and is reported, never trusted.
//! - `first_party` — **not** in the witness. `first_party_row_mismatch` in the
//!   loader refuses a `true` row for a manifest that does not claim
//!   `origin: orbit`, and the validator names tools from the manifest.
//! - `source`, `certified_orbit_version`, timestamps — informational.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_tools::plugin::physical_or_lexical;
use orbit_types::plugin::InstalledPlugin;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::plugin_host::plugin_install_root;

/// Host-owned directory beside the namespace install directories. A namespace
/// must start with a lowercase letter (`is_valid_segment`), so the leading dot
/// cannot collide with one.
const GRANT_WITNESS_DIR: &str = ".grants";

/// Domain separator, so the preimage of one Orbit record is never the preimage
/// of another. Bumping it invalidates every witness, which fails closed.
const GRANT_DIGEST_DOMAIN: &str = "orbit.plugin.grants.v1";

const WITNESS_SCHEMA_VERSION: u32 = 1;

/// What `orbit plugin enable` authorized for one plugin, as the loader checks
/// it back. Written outside `orbit.db` on purpose; see the module docs.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct GrantAuthorization {
    schema_version: u32,
    plugin: String,
    /// [`plugin_grants_digest`] over the authorized set.
    grants_digest: String,
    authorized_at: String,
}

/// Where the witness for `name` lives.
pub fn plugin_grant_witness_path(global_root: &Path, name: &str) -> PathBuf {
    plugin_install_root(global_root)
        .join(GRANT_WITNESS_DIR)
        .join(format!("{name}.json"))
}

/// The integrity value over one authorized grant set.
///
/// Keyed on the namespace and the enable flag as well as the grants, so a
/// witness cannot be replayed onto another plugin or used to flip a disabled
/// plugin back on. Deliberately *not* keyed on the version or the manifest
/// digest: grants legitimately survive `orbit plugin add` of a newer version,
/// and binding either would refuse every upgraded plugin.
///
/// Grants are sorted and deduplicated first, so the value states which grants
/// were authorized and nothing about the order they were recorded in.
pub fn plugin_grants_digest(name: &str, enabled: bool, grants: &[String]) -> String {
    let mut canonical: Vec<&str> = grants.iter().map(String::as_str).collect();
    canonical.sort_unstable();
    canonical.dedup();
    let mut hasher = Sha256::new();
    hasher.update(GRANT_DIGEST_DOMAIN.as_bytes());
    hasher.update(b"\n");
    hasher.update(name.as_bytes());
    hasher.update(b"\n");
    hasher.update(if enabled {
        b"enabled" as &[u8]
    } else {
        b"disabled"
    });
    for grant in canonical {
        hasher.update(b"\n");
        hasher.update(grant.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// Record the grant set an operator just authorized.
///
/// Called after the `plugins` row is written, so a failure here leaves a row
/// whose witness is stale and whose plugin is therefore refused — the error is
/// propagated to the operator rather than swallowed, because the alternative is
/// a plugin that silently will not load.
pub fn record_authorized_grants(
    global_root: &Path,
    name: &str,
    enabled: bool,
    grants: &[String],
) -> Result<(), OrbitError> {
    let path = plugin_grant_witness_path(global_root, name);
    let dir = path.parent().unwrap_or(global_root);
    std::fs::create_dir_all(dir)
        .map_err(|error| OrbitError::Io(format!("create {}: {error}", dir.display())))?;
    let witness = GrantAuthorization {
        schema_version: WITNESS_SCHEMA_VERSION,
        plugin: name.to_string(),
        grants_digest: plugin_grants_digest(name, enabled, grants),
        authorized_at: chrono::Utc::now().to_rfc3339(),
    };
    let body = serde_json::to_string_pretty(&witness).map_err(|error| {
        OrbitError::Execution(format!("serialize grant authorization: {error}"))
    })?;
    std::fs::write(&path, format!("{body}\n"))
        .map_err(|error| OrbitError::Io(format!("write {}: {error}", path.display())))?;
    Ok(())
}

/// Drop the witness when the install goes away, so a later reinstall of the
/// same namespace starts with no authority rather than the old one.
pub fn forget_authorized_grants(global_root: &Path, name: &str) {
    let path = plugin_grant_witness_path(global_root, name);
    if path.exists()
        && let Err(error) = std::fs::remove_file(&path)
    {
        tracing::warn!(
            target: "orbit.core.plugin",
            plugin = %name,
            path = %path.display(),
            "could not remove the grant authorization record: {error}",
        );
    }
}

/// Check one enabled row's grants against what was authorized.
///
/// `Err` is the operator-facing diagnostic: what was found, and the command
/// that re-authorizes it. The caller refuses the plugin and audits the
/// refusal; this function never decides to trust a row it cannot verify.
///
/// A row with no grants and no witness is accepted: nothing was authorized and
/// nothing is claimed, which is the state of every plugin enabled without
/// `--grant` and of every host that has not enabled one yet. A row that *holds*
/// grants without a witness is refused — that is exactly the injected row, and
/// back-filling a witness from the row would authorize the injection.
pub fn verify_recorded_grants(
    global_root: &Path,
    installed: &InstalledPlugin,
) -> Result<(), String> {
    let path = plugin_grant_witness_path(global_root, &installed.name);
    let expected = plugin_grants_digest(&installed.name, installed.enabled, &installed.grants);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if installed.grants.is_empty() {
                return Ok(());
            }
            return Err(unauthorized_message(
                installed,
                &format!(
                    "no authorization record exists at {} (this host has not recorded them, or \
                     the record was removed)",
                    path.display()
                ),
            ));
        }
        Err(error) => {
            return Err(unauthorized_message(
                installed,
                &format!(
                    "its authorization record at {} is unreadable: {error}",
                    path.display()
                ),
            ));
        }
    };
    let witness: GrantAuthorization = serde_json::from_str(&raw).map_err(|error| {
        unauthorized_message(
            installed,
            &format!(
                "its authorization record at {} does not parse: {error}",
                path.display()
            ),
        )
    })?;
    if witness.schema_version != WITNESS_SCHEMA_VERSION {
        return Err(unauthorized_message(
            installed,
            &format!(
                "its authorization record at {} is schema {} and this Orbit reads schema \
                 {WITNESS_SCHEMA_VERSION}",
                path.display(),
                witness.schema_version
            ),
        ));
    }
    if witness.plugin != installed.name {
        return Err(unauthorized_message(
            installed,
            &format!(
                "its authorization record at {} was written for '{}'",
                path.display(),
                witness.plugin
            ),
        ));
    }
    if witness.grants_digest != expected {
        return Err(unauthorized_message(
            installed,
            "its recorded grants do not match the set this host authorized",
        ));
    }
    Ok(())
}

/// Check that one enabled row's `install_path` is a tree this host installed.
///
/// The witness does not cover the path (module docs), so this is what stops a
/// row-writing backend from pointing its authorized grant names at a tree of
/// its own: the path must resolve strictly beneath
/// `<global_root>/plugins/<name>/`, which is read-only to a confined backend.
/// Resolution is physical when the path exists — a `..` or a link cannot
/// place a path beneath a directory it does not live under — and lexical when
/// it does not, so a vanished install is still reported as vanished by the
/// loader rather than as relocated.
///
/// `Err` is the operator-facing diagnostic naming the recorded and the
/// expected path. Like [`verify_recorded_grants`], the caller refuses the
/// plugin and audits the refusal.
pub fn verify_install_path(global_root: &Path, installed: &InstalledPlugin) -> Result<(), String> {
    let expected = plugin_install_root(global_root).join(&installed.name);
    let recorded = Path::new(&installed.install_path);
    // A relative path would resolve against whatever the current directory
    // happens to be; `orbit plugin add` never records one.
    if !recorded.is_absolute() {
        return Err(relocated_message(installed, &expected));
    }
    let resolved = physical_or_lexical(recorded);
    let root = physical_or_lexical(&expected);
    if resolved == root || !resolved.starts_with(&root) {
        return Err(relocated_message(installed, &expected));
    }
    Ok(())
}

/// The install-path counterpart of [`unauthorized_message`]: what the row
/// records, where this host installs, and the commands that settle it.
fn relocated_message(installed: &InstalledPlugin, expected: &Path) -> String {
    format!(
        "plugin '{}' is refused: its recorded install path {} does not resolve beneath {}, the \
         only place this host installs it; grants apply only to a tree `orbit plugin add` placed \
         there, so this Orbit will not load the plugin from the recorded path. Reinstall it with \
         `orbit plugin add`, or run `orbit plugin remove {}` if you did not install it.",
        installed.name,
        installed.install_path,
        expected.display(),
        installed.name,
    )
}

/// One message, naming what is wrong, what the row currently claims, and the
/// command that settles it — the caller reports it as this plugin's single
/// diagnostic and as the audit row's error.
fn unauthorized_message(installed: &InstalledPlugin, reason: &str) -> String {
    let claimed = if installed.grants.is_empty() {
        "no grants".to_string()
    } else {
        installed
            .grants
            .iter()
            .map(|grant| format!("`{grant}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "plugin '{}' is refused: {reason}. The stored record claims {claimed}; grants are only \
         authority when `orbit plugin enable` recorded them, so this Orbit will not register the \
         plugin with them. Run `orbit plugin enable {} --grant <grants>` to authorize the set you \
         intend, or `orbit plugin remove {}` if you did not install it.",
        installed.name, installed.name, installed.name,
    )
}
