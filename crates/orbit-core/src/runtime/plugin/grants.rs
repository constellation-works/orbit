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
//! the child cannot read, and the `orbit_tools` read grant was the whole global
//! root when this was designed — a key kept there was a key the child could
//! read and then forge with. What bounds the attacker here is the write
//! boundary, not a secret. The witness directory is unreadable to a confined
//! backend today (each is granted only its own witness, which it needs to
//! verify its own row), but that is defence in depth: it keeps one plugin from
//! reading what another was authorized for, and the digest is still not a
//! secret [ORB-12798].
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
//!   [ORB-12785], and so does every lifecycle verb that would read or delete
//!   that tree [ORB-12800]. Without this, a row could keep its authorized
//!   grant names and point them at a tree the backend wrote under one of its
//!   own write roots, and the witness would still match.
//! - `manifest_digest` — **not** in the witness, deliberately: grants survive
//!   `orbit plugin add` of a newer version only when its permission requests
//!   did not widen; a widening revokes the witness and requires re-consent.
//!   Binding the digest here would refuse every safe upgrade. The digest is
//!   only meaningful *because* `install_path` is bound:
//!   the loader compares it against the bytes at a path the attacker cannot
//!   populate, so agreeing with it proves the tree is the one `orbit plugin
//!   add` copied there (§4.1).
//! - `version` — **not** in the witness, for the same upgrade reason; it names
//!   the install directory and is reported, never trusted.
//! - `first_party` — **not** in the witness. `first_party_row_mismatch` in the
//!   loader refuses a `true` row for a manifest that does not claim
//!   `origin: orbit`, and the validator names tools from the manifest.
//! - `source`, `certified_orbit_version`, timestamps — informational.
//!
//! # Resolved programs
//!
//! The witness also carries the canonical path each `requires.programs`
//! entry resolved to when the operator enabled the plugin. Those paths are
//! the consent itself, not a claim checked against the row, so they are not
//! in the digest: they live only in this host-owned file, which a confined
//! backend cannot write, and the sandbox grants them read and execute
//! whatever `PATH` the spawning caller has. Changing one means writing this
//! file again, which only an enabling command does — a changed resolution is
//! a re-consent event.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_tools::plugin::physical_with_missing_tail;
use orbit_types::plugin::{InstalledPlugin, is_valid_namespace};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::paths::{plugin_install_root, plugin_namespace_dir};

/// Host-owned directory beside the namespace install directories. A namespace
/// must start with a lowercase letter (`is_valid_segment`), so the leading dot
/// cannot collide with one. Named in `orbit-tools` because the plugin sandbox
/// decides who may read it: the directory is denied to every confined backend,
/// which is granted its own witness file and no other.
const GRANT_WITNESS_DIR: &str = orbit_tools::plugin::PLUGIN_GRANT_WITNESS_DIR;
/// Reserved leaf for invalid row names. A leading dot cannot be a valid
/// namespace, which must start with a lowercase letter.
const INVALID_GRANT_WITNESS_FILE: &str = ".invalid.json";

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
    /// Declared program name → the canonical path it resolved to at consent.
    /// Absent from witnesses written before programs were resolved, which
    /// therefore grant no program (see the module docs).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    programs: BTreeMap<String, PathBuf>,
}

/// Where the witness for `name` lives.
///
/// Only a valid plugin namespace becomes a path component. Invalid names map
/// to a reserved leaf that cannot collide with a valid namespace; callers that
/// write or verify witnesses reject invalid names before using this path.
pub fn plugin_grant_witness_path(global_root: &Path, name: &str) -> PathBuf {
    let file_name = if is_valid_namespace(name) {
        format!("{name}.json")
    } else {
        INVALID_GRANT_WITNESS_FILE.to_string()
    };
    plugin_install_root(global_root)
        .join(GRANT_WITNESS_DIR)
        .join(file_name)
}

/// The integrity value over one authorized grant set.
///
/// Keyed on the namespace and the enable flag as well as the grants, so a
/// witness cannot be replayed onto another plugin or used to flip a disabled
/// plugin back on. Deliberately *not* keyed on the version or the manifest
/// digest: grants legitimately survive a safe `orbit plugin add` of a newer
/// version whose permission requests did not widen, and binding either would
/// refuse every upgraded plugin.
///
/// Grants are sorted and deduplicated first, so the value states which grants
/// were authorized and nothing about the order they were recorded in.
///
/// A path-scoped grant carries its roots in the string it is recorded as
/// (`fs=/srv/data,/srv/cache`), so the roots are inside the digest without
/// this function knowing the grammar: re-scoping a plugin changes its
/// recorded set, which revokes the witness and requires fresh consent, and a
/// backend that widened its own roots in the row is refused at the next load
/// exactly as one that added a grant is [ORB-12840].
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
    record_authorization(global_root, name, enabled, grants, &BTreeMap::new())
}

/// [`record_authorized_grants`], plus the program paths this consent
/// resolved `requires.programs` to (module docs).
pub fn record_authorization(
    global_root: &Path,
    name: &str,
    enabled: bool,
    grants: &[String],
    programs: &BTreeMap<String, PathBuf>,
) -> Result<(), OrbitError> {
    if !is_valid_namespace(name) {
        return Err(OrbitError::Execution(format!(
            "cannot record grant authorization for invalid plugin namespace '{name}'"
        )));
    }
    let path = plugin_grant_witness_path(global_root, name);
    let witness = GrantAuthorization {
        schema_version: WITNESS_SCHEMA_VERSION,
        plugin: name.to_string(),
        grants_digest: plugin_grants_digest(name, enabled, grants),
        authorized_at: chrono::Utc::now().to_rfc3339(),
        programs: programs.clone(),
    };
    let body = serde_json::to_string_pretty(&witness).map_err(|error| {
        OrbitError::Execution(format!("serialize grant authorization: {error}"))
    })?;
    // Rename-into-place, so a crash mid-write leaves either the previous
    // witness or the new one, never a truncated file that refuses the plugin
    // until an operator notices and re-runs `orbit plugin enable`. Creates
    // `dir` as needed, same as the old explicit `create_dir_all`.
    atomic_write_text(&path, &format!("{body}\n"))
        .map_err(|error| OrbitError::Io(format!("write {}: {error}", path.display())))?;
    Ok(())
}

/// The program paths the last enabling command recorded for `name`, empty
/// when there is no readable witness written for it.
///
/// Read without judging the grant set: a row whose grants do not match its
/// witness is refused whole by the loader, so no backend is built from it.
pub fn recorded_program_paths(global_root: &Path, name: &str) -> BTreeMap<String, PathBuf> {
    if !is_valid_namespace(name) {
        return BTreeMap::new();
    }
    std::fs::read_to_string(plugin_grant_witness_path(global_root, name))
        .ok()
        .and_then(|raw| serde_json::from_str::<GrantAuthorization>(&raw).ok())
        .filter(|witness| {
            witness.schema_version == WITNESS_SCHEMA_VERSION && witness.plugin == name
        })
        .map(|witness| witness.programs)
        .unwrap_or_default()
}

/// Drop the witness when the install goes away, so a later reinstall of the
/// same namespace starts with no authority rather than the old one.
pub fn forget_authorized_grants(global_root: &Path, name: &str) {
    if !is_valid_namespace(name) {
        return;
    }
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
    if !is_valid_namespace(&installed.name) {
        return Err(unauthorized_message(
            installed,
            "its plugin namespace is invalid, so this Orbit will not read an authorization record for it",
        ));
    }
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

/// Check that one row's `install_path` is a tree this host installed.
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
/// Every consumer that reads or writes the recorded tree holds it to this
/// check, not only the loader: `orbit plugin enable`, `disable` and `remove`
/// refuse a row that fails it rather than seeding from, unlinking by, or
/// deleting a path this host did not install [ORB-12800].
///
/// `Err` is the operator-facing diagnostic naming the recorded and the
/// expected path. Like [`verify_recorded_grants`], the caller refuses the
/// plugin and audits the refusal.
pub fn verify_install_path(global_root: &Path, installed: &InstalledPlugin) -> Result<(), String> {
    let expected = plugin_namespace_dir(global_root, &installed.name);
    let recorded = Path::new(&installed.install_path);
    // A relative path would resolve against whatever the current directory
    // happens to be; `orbit plugin add` never records one.
    if !recorded.is_absolute() {
        return Err(relocated_message(installed, &expected));
    }
    let resolved = physical_with_missing_tail(recorded);
    let root = physical_with_missing_tail(&expected);
    if resolved == root || !resolved.starts_with(&root) {
        return Err(relocated_message(installed, &expected));
    }
    Ok(())
}

/// The install-path counterpart of [`unauthorized_message`]: what the row
/// records, where this host installs, and the commands that settle it.
///
/// The remediation it names has to be a command that does not itself follow
/// the recorded path: `orbit plugin remove <ns>` deletes the recorded tree, so
/// recommending it would turn this diagnostic into the deletion of whatever
/// the row points at. `--record-only` is the verb that drops Orbit's record of
/// the plugin and leaves the recorded path alone [ORB-12800].
fn relocated_message(installed: &InstalledPlugin, expected: &Path) -> String {
    format!(
        "plugin '{}' is refused: its recorded install path {} does not resolve beneath {}, the \
         only place this host installs it; grants apply only to a tree `orbit plugin add` placed \
         there, so this Orbit will not run the plugin from the recorded path, and no plugin \
         command will write to or delete anything under it. Reinstall it with `orbit plugin add`, \
         or run `orbit plugin remove {} --yes --record-only` to drop this host's record of it \
         without touching the recorded path.",
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
