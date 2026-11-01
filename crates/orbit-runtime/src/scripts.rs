//! Script grants: the operator's host-local consent to run one workspace
//! script at one content digest (script steps phase 1 — nothing executes
//! yet; operators can grant and see staleness first).
//!
//! A grant is two halves on opposite sides of a write boundary, exactly
//! as plugin grants are: the `script_grant` row in `orbit.db`, which a
//! caller holding `orbit_tools` can write, and a witness at
//! `<global_root>/scripts/.grants/<workspace_id>/<name>.json`, which only
//! the granting command writes. The witness carries an integrity digest
//! over the grant record plus a copy of the granted manifest and entry
//! bytes (hex), so a later phase can show the exact re-grant diff. A row
//! whose witness is missing or does not match is refused: it is either a
//! forged row or a grant whose authorizing write never completed, and
//! both fail closed.
//!
//! Granting and revoking are governed operator-only operations
//! (`orbit.script.grant` / `orbit.script.revoke`); the chokepoint refuses
//! agent sessions and plugin callbacks before this code is reached.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_engine::activity_job::{ScriptAsset, V2ScriptCatalog};
use orbit_types::identity::lower_hex;
use orbit_types::workflow::activity_job::{
    SCRIPT_OUTPUT_HARD_MAX_FILE_BYTES, SCRIPT_OUTPUT_HARD_MAX_FILES, ScriptGrant,
    ScriptGrantLimits, ScriptHostRole, ScriptNetwork, is_valid_script_name, script_content_digest,
    script_grant_id, script_sha256_hex,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::RuntimeKernel;

const WITNESS_SCHEMA_VERSION: u32 = 1;
/// Witness format version string. A witness with an unknown
/// `orbit.script.grant.v<N>` version refuses, per the compatibility rule.
const WITNESS_VERSION: &str = "orbit.script.grant.v1";
const WITNESS_INTEGRITY_DOMAIN: &str = "orbit.script.grant.witness.v1";
const INVALID_WITNESS_FILE: &str = ".invalid.json";

/// What the granting command authorized, as the loader checks it back.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScriptGrantWitness {
    schema_version: u32,
    version: String,
    grant: ScriptGrant,
    /// Digest over the canonical grant record above.
    integrity: String,
    /// The granted manifest bytes, hex, for the re-grant diff.
    manifest_hex: String,
    /// The granted entry bytes, hex.
    entry_hex: String,
}

fn witness_integrity(grant: &ScriptGrant) -> Result<String, OrbitError> {
    let canonical = serde_json::to_string(grant).map_err(|error| {
        OrbitError::Execution(format!("serialize script grant for witness: {error}"))
    })?;
    let mut hasher = Sha256::new();
    hasher.update(WITNESS_INTEGRITY_DOMAIN.as_bytes());
    hasher.update(b"\n");
    hasher.update(canonical.as_bytes());
    Ok(lower_hex(hasher.finalize()))
}

/// One path component for a workspace id: the id itself when it is a plain
/// segment, otherwise a digest of it, so a stored id can never place the
/// witness outside its directory.
fn witness_workspace_component(workspace_id: &str) -> String {
    let plain = !workspace_id.is_empty()
        && workspace_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if plain {
        workspace_id.to_string()
    } else {
        script_sha256_hex(workspace_id.as_bytes())
    }
}

/// Where the witness for one workspace script lives.
pub fn script_grant_witness_path(global_root: &Path, workspace_id: &str, name: &str) -> PathBuf {
    let file_name = if is_valid_script_name(name) {
        format!("{name}.json")
    } else {
        INVALID_WITNESS_FILE.to_string()
    };
    global_root
        .join("scripts")
        .join(".grants")
        .join(witness_workspace_component(workspace_id))
        .join(file_name)
}

/// Record the witness half of a grant the operator just authorized.
pub fn record_script_grant_witness(
    global_root: &Path,
    grant: &ScriptGrant,
    manifest_bytes: &[u8],
    entry_bytes: &[u8],
) -> Result<(), OrbitError> {
    if !is_valid_script_name(&grant.name) {
        return Err(OrbitError::Execution(format!(
            "cannot record a script grant witness for invalid script name '{}'",
            grant.name
        )));
    }
    let witness = ScriptGrantWitness {
        schema_version: WITNESS_SCHEMA_VERSION,
        version: WITNESS_VERSION.to_string(),
        integrity: witness_integrity(grant)?,
        manifest_hex: lower_hex(manifest_bytes),
        entry_hex: lower_hex(entry_bytes),
        grant: grant.clone(),
    };
    let path = script_grant_witness_path(global_root, &grant.workspace_id, &grant.name);
    let body = serde_json::to_string_pretty(&witness).map_err(|error| {
        OrbitError::Execution(format!("serialize script grant witness: {error}"))
    })?;
    atomic_write_text(&path, &format!("{body}\n"))
        .map_err(|error| OrbitError::Io(format!("write {}: {error}", path.display())))?;
    Ok(())
}

/// Drop a grant's witness, so a later row of the same name starts with no
/// authority rather than the revoked one.
pub fn forget_script_grant_witness(global_root: &Path, workspace_id: &str, name: &str) {
    if !is_valid_script_name(name) {
        return;
    }
    let path = script_grant_witness_path(global_root, workspace_id, name);
    if path.exists()
        && let Err(error) = std::fs::remove_file(&path)
    {
        tracing::warn!(
            target: "orbit.runtime.scripts",
            script = %name,
            path = %path.display(),
            "could not remove the script grant witness: {error}",
        );
    }
}

/// Check one grant row against its witness. `Err` is the operator-facing
/// refusal; the caller treats the grant as no authority at all.
pub fn verify_script_grant_witness(global_root: &Path, grant: &ScriptGrant) -> Result<(), String> {
    let refused = |reason: String| {
        format!(
            "script '{}' grant {} is refused: {reason}. A grant row is authority only with \
             the witness `orbit script grant` recorded. Run `orbit script grant {}` to \
             authorize the current bytes, or `orbit script revoke {}` to drop the record.",
            grant.name, grant.grant_id, grant.name, grant.name
        )
    };
    if !is_valid_script_name(&grant.name) {
        return Err(refused("its script name is invalid".to_string()));
    }
    let path = script_grant_witness_path(global_root, &grant.workspace_id, &grant.name);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(refused(format!(
                "no authorization witness exists at {}",
                path.display()
            )));
        }
        Err(error) => {
            return Err(refused(format!(
                "its witness at {} is unreadable: {error}",
                path.display()
            )));
        }
    };
    let witness: ScriptGrantWitness = serde_json::from_str(&raw).map_err(|error| {
        refused(format!(
            "its witness at {} does not parse: {error}",
            path.display()
        ))
    })?;
    if witness.schema_version != WITNESS_SCHEMA_VERSION || witness.version != WITNESS_VERSION {
        return Err(refused(format!(
            "its witness at {} is version {} (schema {}) and this Orbit reads {WITNESS_VERSION}",
            path.display(),
            witness.version,
            witness.schema_version
        )));
    }
    let expected = witness_integrity(&witness.grant)
        .map_err(|error| refused(format!("its witness grant does not serialize: {error}")))?;
    if witness.integrity != expected {
        return Err(refused(
            "its witness integrity value does not match its grant record".to_string(),
        ));
    }
    if witness.grant != *grant {
        return Err(refused(
            "the stored grant row does not match the grant its witness authorized".to_string(),
        ));
    }
    Ok(())
}

/// One script's grant standing, as `orbit script list`, `orbit script
/// show` and doctor report it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptGrantState {
    /// A verified grant whose digest matches the current bytes.
    Granted,
    /// A verified grant whose digest no longer matches (either file was
    /// edited after granting).
    Stale,
    /// A manifest with no grant.
    Ungranted,
    /// A grant whose manifest no longer exists.
    Orphaned,
    /// A grant row whose witness is missing or does not match.
    WitnessRefused,
}

impl ScriptGrantState {
    pub fn as_str(self) -> &'static str {
        match self {
            ScriptGrantState::Granted => "granted",
            ScriptGrantState::Stale => "stale",
            ScriptGrantState::Ungranted => "ungranted",
            ScriptGrantState::Orphaned => "orphaned",
            ScriptGrantState::WitnessRefused => "witness_refused",
        }
    }
}

impl std::fmt::Display for ScriptGrantState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One script as the operator surfaces see it: its manifest facts when it
/// has one, its grant when it has one, and the state joining the two.
#[derive(Debug, Clone)]
pub struct ScriptStatus {
    pub name: String,
    pub state: ScriptGrantState,
    /// The loaded manifest, when the workspace still has one.
    pub asset: Option<ScriptAsset>,
    /// The stored grant row, when one exists. Present but untrusted in
    /// the `WitnessRefused` state.
    pub grant: Option<ScriptGrant>,
    /// Why the state is what it is, naming both digests when stale.
    pub detail: String,
}

/// Capability choices for one `orbit script grant`. Anything unset takes
/// the manifest's request, so granting what a script asks for is the
/// plain `orbit script grant <name>`; flags widen or override it.
#[derive(Debug, Clone, Default)]
pub struct ScriptGrantOptions {
    pub network: Option<ScriptNetwork>,
    pub read_roots: Vec<PathBuf>,
    pub write_roots: Vec<PathBuf>,
    pub env_pass: Vec<String>,
    pub follower: bool,
    /// The most output files one step may keep. Unset keeps the default
    /// ceiling; a re-grant never inherits or widens it on its own.
    pub output_max_files: Option<u32>,
    /// The largest output file one step may keep, in bytes.
    pub output_max_file_bytes: Option<u64>,
}

/// The output limits one grant records: the defaults, with each
/// acknowledged override checked against the hard ceilings.
fn grant_output_limits(
    name: &str,
    options: &ScriptGrantOptions,
) -> Result<ScriptGrantLimits, OrbitError> {
    let mut limits = ScriptGrantLimits::default();
    if let Some(files) = options.output_max_files {
        if !(1..=SCRIPT_OUTPUT_HARD_MAX_FILES).contains(&files) {
            return Err(OrbitError::InvalidInput(format!(
                "script `{name}` output file count {files} is outside 1..={SCRIPT_OUTPUT_HARD_MAX_FILES}; \
                 nothing was granted"
            )));
        }
        limits.output_max_files = files;
    }
    if let Some(bytes) = options.output_max_file_bytes {
        if !(1..=SCRIPT_OUTPUT_HARD_MAX_FILE_BYTES).contains(&bytes) {
            return Err(OrbitError::InvalidInput(format!(
                "script `{name}` output file size {bytes} is outside \
                 1..={SCRIPT_OUTPUT_HARD_MAX_FILE_BYTES} bytes; nothing was granted"
            )));
        }
        limits.output_max_file_bytes = bytes;
    }
    Ok(limits)
}

impl RuntimeKernel {
    /// Load this workspace's script catalog. Scripts are workspace-only:
    /// the global root's `resources/` holds none.
    pub fn v2_script_catalog(&self) -> Result<V2ScriptCatalog, OrbitError> {
        let paths = self.paths();
        let mut catalog = V2ScriptCatalog::new(paths.scripts_dir.clone(), paths.repo_root.clone());
        catalog
            .load_dir()
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
        Ok(catalog)
    }

    /// Every script this workspace declares or holds a grant for, with its
    /// grant standing.
    pub fn script_statuses(&self) -> Result<Vec<ScriptStatus>, OrbitError> {
        let workspace_id = self.workspace_id()?;
        let global_root = self.global_root();
        let catalog = self.v2_script_catalog()?;
        let grants = self.sqlite_store()?.list_script_grants(&workspace_id)?;
        let mut statuses: Vec<ScriptStatus> = Vec::new();
        for asset in catalog.iter() {
            let grant = grants
                .iter()
                .find(|grant| grant.name == asset.name)
                .cloned();
            let (state, detail) = match &grant {
                None => (
                    ScriptGrantState::Ungranted,
                    format!(
                        "no grant recorded; run `orbit script grant {}` to authorize digest {}",
                        asset.name, asset.digest
                    ),
                ),
                Some(grant) => match verify_script_grant_witness(&global_root, grant) {
                    Err(reason) => (ScriptGrantState::WitnessRefused, reason),
                    Ok(()) if grant.digest == asset.digest => (
                        ScriptGrantState::Granted,
                        format!(
                            "grant {} matches the current bytes at digest {}",
                            grant.grant_id, asset.digest
                        ),
                    ),
                    Ok(()) => (
                        ScriptGrantState::Stale,
                        format!(
                            "grant {} was made at digest {} (manifest {}, entry {}) but the \
                             current bytes digest to {} (manifest {}, entry {}); re-grant \
                             after reviewing the change",
                            grant.grant_id,
                            grant.digest,
                            grant.manifest_digest,
                            grant.entry_digest,
                            asset.digest,
                            asset.manifest_digest,
                            asset.entry_digest
                        ),
                    ),
                },
            };
            statuses.push(ScriptStatus {
                name: asset.name.clone(),
                state,
                asset: Some(asset.clone()),
                grant,
                detail,
            });
        }
        for grant in grants {
            if catalog.get(&grant.name).is_some() {
                continue;
            }
            let witness_note = match verify_script_grant_witness(&global_root, &grant) {
                Ok(()) => String::new(),
                Err(reason) => format!(" Its witness also fails verification: {reason}"),
            };
            statuses.push(ScriptStatus {
                name: grant.name.clone(),
                state: ScriptGrantState::Orphaned,
                asset: None,
                detail: format!(
                    "grant {} at digest {} has no manifest at {}; run `orbit script revoke {}` \
                     to drop it.{witness_note}",
                    grant.grant_id,
                    grant.digest,
                    catalog.manifest_path_for(&grant.name).display(),
                    grant.name,
                ),
                grant: Some(grant),
            });
        }
        statuses.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(statuses)
    }

    /// One script's standing, or `None` when the workspace has neither a
    /// manifest nor a grant under `name`.
    pub fn script_status(&self, name: &str) -> Result<Option<ScriptStatus>, OrbitError> {
        Ok(self
            .script_statuses()?
            .into_iter()
            .find(|status| status.name == name))
    }

    /// The grant a script name currently holds, verified against its
    /// witness. A row without a matching witness is refused, never
    /// returned as authority.
    pub fn verified_script_grant(&self, name: &str) -> Result<Option<ScriptGrant>, OrbitError> {
        let workspace_id = self.workspace_id()?;
        let Some(grant) = self.sqlite_store()?.get_script_grant(&workspace_id, name)? else {
            return Ok(None);
        };
        verify_script_grant_witness(&self.global_root(), &grant).map_err(OrbitError::Execution)?;
        Ok(Some(grant))
    }

    /// Grant one script at its current digest. The row is written first
    /// and the witness second: a failure between them leaves a row its
    /// witness does not authorize, which every reader refuses.
    pub fn grant_script(
        &self,
        name: &str,
        options: &ScriptGrantOptions,
    ) -> Result<ScriptGrant, OrbitError> {
        let catalog = self.v2_script_catalog()?;
        let Some(asset) = catalog.get(name) else {
            return Err(OrbitError::InvalidInput(format!(
                "script `{name}` manifest not found at {}; nothing was granted",
                catalog.manifest_path_for(name).display()
            )));
        };
        let manifest_bytes = std::fs::read(&asset.manifest_path).map_err(|error| {
            OrbitError::Io(format!("read {}: {error}", asset.manifest_path.display()))
        })?;
        let entry_bytes = std::fs::read(&asset.entry_path).map_err(|error| {
            OrbitError::Io(format!("read {}: {error}", asset.entry_path.display()))
        })?;
        let digest = script_content_digest(&manifest_bytes, &entry_bytes);
        if digest != asset.digest {
            return Err(OrbitError::InvalidInput(format!(
                "script `{name}` changed at {} while it was being granted; run the grant again",
                asset.manifest_path.display()
            )));
        }
        let limits = grant_output_limits(name, options)?;
        let interpreter_paths = resolve_grant_interpreter_paths(asset)?;
        let mut read_roots = asset.spec.requests.read_roots.clone();
        read_roots.extend(options.read_roots.iter().cloned());
        let mut write_roots = asset.spec.requests.write_roots.clone();
        write_roots.extend(options.write_roots.iter().cloned());
        for root in read_roots.iter().chain(write_roots.iter()) {
            if !root.is_absolute() {
                return Err(OrbitError::InvalidInput(format!(
                    "script `{name}` grant root {} is not absolute; grant roots are recorded as absolute paths",
                    root.display()
                )));
            }
        }
        let mut env_pass = asset.spec.requests.env_pass.clone();
        for env_name in &options.env_pass {
            if !env_pass.contains(env_name) {
                env_pass.push(env_name.clone());
            }
        }
        let workspace_id = self.workspace_id()?;
        let grant = ScriptGrant {
            grant_id: script_grant_id(&workspace_id, name, &digest),
            workspace_id,
            name: name.to_string(),
            manifest_digest: script_sha256_hex(&manifest_bytes),
            entry_digest: script_sha256_hex(&entry_bytes),
            digest,
            interpreter_paths,
            network: options.network.unwrap_or(asset.spec.requests.network),
            read_roots,
            write_roots,
            env_pass,
            limits,
            host_role: if options.follower {
                ScriptHostRole::Follower
            } else {
                ScriptHostRole::Owner
            },
            grantor: self.actor_label().to_string(),
            granted_at: chrono::Utc::now().to_rfc3339(),
            provenance: "cli".to_string(),
            expires_at: None,
        };
        self.sqlite_store()?.upsert_script_grant(&grant)?;
        record_script_grant_witness(&self.global_root(), &grant, &manifest_bytes, &entry_bytes)?;
        Ok(grant)
    }

    /// Revoke one script's grant: drop the row and its witness. Returns
    /// whether a row existed.
    pub fn revoke_script(&self, name: &str) -> Result<bool, OrbitError> {
        if !is_valid_script_name(name) {
            return Err(OrbitError::InvalidInput(format!(
                "invalid script name `{name}`"
            )));
        }
        let workspace_id = self.workspace_id()?;
        let deleted = self
            .sqlite_store()?
            .delete_script_grant(&workspace_id, name)?;
        forget_script_grant_witness(&self.global_root(), &workspace_id, name);
        Ok(deleted)
    }
}

/// The interpreter argv a grant records: the first element resolved to
/// its canonical path on the granting operator's `PATH`, the rest
/// literal. An interpreter that does not resolve refuses the grant —
/// recording nothing would let a later caller's `PATH` decide what runs.
fn resolve_grant_interpreter_paths(asset: &ScriptAsset) -> Result<Vec<PathBuf>, OrbitError> {
    let Some((program, rest)) = asset.interpreter.split_first() else {
        return Ok(Vec::new());
    };
    let (resolved, unresolved) = orbit_tools::plugin::resolve_declared_programs(
        std::slice::from_ref(program),
        std::env::var_os("PATH").as_deref(),
    );
    if let Some((_, reason)) = unresolved.into_iter().next() {
        return Err(OrbitError::InvalidInput(format!(
            "script `{}` interpreter `{program}` did not resolve at {}: {reason}; make it \
             resolve on PATH (or declare its absolute path), then grant again",
            asset.name,
            asset.manifest_path.display()
        )));
    }
    let canonical = resolved.get(program).cloned().ok_or_else(|| {
        OrbitError::Execution(format!(
            "script `{}` interpreter `{program}` resolved to nothing",
            asset.name
        ))
    })?;
    let mut paths = vec![canonical];
    paths.extend(rest.iter().map(PathBuf::from));
    Ok(paths)
}
