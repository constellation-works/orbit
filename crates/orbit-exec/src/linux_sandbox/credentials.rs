//! Well-known credential locations hidden from every Bubblewrap child.
//!
//! The backend keeps the host filesystem readable (`--ro-bind / /`), so
//! without this a confined worker could read `~/.ssh`, `~/.aws`, the `gh`
//! token and a cargo publish token. The macOS profile denies the same list;
//! [`crate::credential_paths`] is the single source of truth for both.
//!
//! Each existing location is masked after every other mount of the plan, so no
//! policy grant or alias bind can expose it again: a directory is replaced by
//! an empty `--tmpfs`, a file by `--ro-bind /dev/null`. A location that does
//! not exist is skipped, because Bubblewrap cannot mount over a missing
//! destination. A location the child could also reach through a second path
//! (a bind mount alias), or one that would hide a path the plan grants the
//! child, refuses the plan rather than starting with an incomplete mask.

use super::mask::{host_alias, parse_mountinfo, plan_alias};
use super::*;
use crate::credential_paths::credential_read_denies;

/// Credential locations to mask, resolved from this process's `HOME` and
/// `CARGO_HOME`. Empty off Linux, where a plan is only ever compiled for
/// tests and never handed to Bubblewrap.
pub(super) fn host_credential_denies() -> Vec<CredentialReadDeny> {
    if !cfg!(target_os = "linux") {
        return Vec::new();
    }
    let home = std::env::var_os("HOME");
    let cargo_home = std::env::var_os("CARGO_HOME");
    credential_read_denies(home.as_deref(), cargo_home.as_deref())
}

/// Append one mask mount per existing credential location.
///
/// `mounts` is the host mount table used to detect a second path to a masked
/// location; it is read lazily, only once a location actually exists.
pub(super) fn append_credential_masks(
    out: &mut Vec<String>,
    denies: &[CredentialReadDeny],
    mounts: impl FnOnce() -> Result<Vec<MountEntry>, OrbitError>,
) -> Result<(), OrbitError> {
    let mut targets = BTreeSet::new();
    for deny in denies {
        // A dangling or absent path resolves to nothing: nothing to hide.
        let Ok(canonical) = std::fs::canonicalize(&deny.path) else {
            continue;
        };
        if canonical.is_dir() || (deny.file && canonical.is_file()) {
            targets.insert(canonical);
        }
    }
    if targets.is_empty() {
        return Ok(());
    }
    let mounts = mounts()?;
    for target in targets {
        if let Some(granted) = granted_path_inside(out, &target) {
            return Err(OrbitError::PolicyDenied(format!(
                "linux-bwrap cannot hide credential location `{}`: the sandbox is granted `{}` \
                 inside it; refusing to start rather than hide the granted path",
                target.display(),
                granted.display()
            )));
        }
        let alias = plan_alias(out, &target).or(host_alias(&mounts, &target)?);
        if let Some(alias) = alias {
            return Err(OrbitError::PolicyDenied(format!(
                "linux-bwrap cannot mask credential location `{}`: the sandbox would also reach \
                 it at `{}`; refusing to start with the mask incomplete",
                target.display(),
                alias.display()
            )));
        }
        if target.is_dir() {
            out.extend(["--tmpfs".to_string(), target.display().to_string()]);
        } else {
            out.extend([
                "--ro-bind".to_string(),
                "/dev/null".to_string(),
                target.display().to_string(),
            ]);
        }
    }
    Ok(())
}

/// Read the host mount table the recursive `--ro-bind / /` hands the child.
pub(super) fn host_mounts() -> Result<Vec<MountEntry>, OrbitError> {
    let text = std::fs::read_to_string("/proc/self/mountinfo").map_err(|error| {
        OrbitError::Execution(format!(
            "read /proc/self/mountinfo to check credential locations for aliases: {error}"
        ))
    })?;
    Ok(parse_mountinfo(&text))
}

/// A path the plan binds onto itself (a writable or read-only grant) that lies
/// at or inside `target`. Masking the target would hide it from the child.
fn granted_path_inside(args: &[String], target: &Path) -> Option<PathBuf> {
    args.windows(3)
        .filter(|triple| matches!(triple[0].as_str(), "--bind" | "--ro-bind"))
        .filter(|triple| triple[1] == triple[2] && triple[1] != "/")
        .map(|triple| PathBuf::from(&triple[1]))
        .find(|path| path.starts_with(target))
}
