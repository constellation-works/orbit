//! Generation admission: which authorities an update locks, and pinning the
//! candidate in each.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;

/// Every generation authority that can hold a live pin on the host binary.
///
/// The invocation's own resolution comes first — `--root`, then `ORBIT_ROOT`,
/// otherwise the host-global root (`~/.orbit`, or `ORBIT_REGISTRY_ROOT` in a
/// managed run) — because that is the authority this process would pin as a
/// client. The host-global root follows whenever the override named something
/// else: what `orbit update` replaces is `current_exe()`, which no root
/// override moves, and every client started without an override pins the
/// host-global root. The initialized workspace selected from the current
/// directory also needs admission: convergence runs there even when its root
/// differs from both the invocation and host-global roots.
///
/// Identical authorities spelled differently collapse to one entry: flock
/// would treat a second open of the same lock as a foreign holder and refuse
/// the update against itself.
pub fn admission_authorities(
    root_override: Option<&Path>,
    workspace_root: Option<&Path>,
) -> Result<Vec<PathBuf>, OrbitError> {
    let resolved = orbit_core::runtime::resolve_generation_root(root_override)?;
    let host_global = orbit_core::runtime::resolve_global_root()?;
    let mut roots = Vec::with_capacity(3);
    push_unique_authority(&mut roots, resolved)?;
    push_unique_authority(&mut roots, host_global)?;
    if let Some(workspace_root) = workspace_root {
        push_unique_authority(&mut roots, workspace_root.to_path_buf())?;
    }
    Ok(roots)
}

fn push_unique_authority(roots: &mut Vec<PathBuf>, root: PathBuf) -> Result<(), OrbitError> {
    let identity = orbit_common::fs::generation::authority_root(&root)?;
    for existing in roots.iter() {
        if orbit_common::fs::generation::authority_root(existing)? == identity {
            return Ok(());
        }
    }
    roots.push(root);
    Ok(())
}

/// Take exclusive admission on every authority, refusing if any is live.
///
/// Returns one admission per root, in the same order: with an override in play
/// the operator otherwise cannot tell which set of clients to quiesce, so both
/// this refusal and a later pin failure name the authority they came from.
///
/// Each admission is also asked up front whether it could record a candidate
/// generation at all. Writability is a property of the record rather than of
/// the lock, and `pin` only runs after the executable has been replaced: a
/// host-global `~/.orbit` on a read-only mount would otherwise let an override
/// invocation swap the binary and then strand every host-global client behind
/// a record naming the generation that is gone. Refusing here also makes
/// `--preflight`, which takes the same admissions, answer for the pin.
pub fn acquire_admissions(
    roots: &[PathBuf],
) -> Result<Vec<orbit_common::fs::generation::GenerationUpdate>, OrbitError> {
    if roots.is_empty() {
        return Err(OrbitError::InvalidInput(
            "no generation authority to admit against; refusing to replace an executable \
             no live client could be observed through"
                .to_string(),
        ));
    }
    roots
        .iter()
        .map(|root| {
            let admission = orbit_common::fs::generation::GenerationUpdate::acquire(root)
                .map_err(|error| naming_authority(root, &error))?;
            admission
                .ensure_can_record()
                .map_err(|error| naming_authority(root, &error))?;
            Ok(admission)
        })
        .collect()
}

/// Pin the candidate generation in every authority admission was taken on.
///
/// `admissions` is what [`acquire_admissions`] returned for `roots`, so the
/// two are index-aligned.
pub(super) fn pin_candidate(
    roots: &[PathBuf],
    admissions: Vec<orbit_common::fs::generation::GenerationUpdate>,
    digest: &str,
    identity: Option<&orbit_common::fs::generation::CompatibilityIdentity>,
) -> Result<Vec<orbit_common::fs::generation::GenerationGuard>, OrbitError> {
    roots
        .iter()
        .zip(admissions)
        .map(|(root, admission)| {
            admission
                .pin(digest, identity)
                .map_err(|error| naming_authority(root, &error))
        })
        .collect()
}

/// Say which authority an admission failure came from.
fn naming_authority(root: &Path, error: &OrbitError) -> OrbitError {
    OrbitError::Execution(format!(
        "{error}. The generation authority is '{}'",
        root.display()
    ))
}
