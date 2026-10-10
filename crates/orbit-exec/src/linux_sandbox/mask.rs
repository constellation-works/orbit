//! Directories and files hidden from a Bubblewrap child behind a read-only
//! stand-in.
//!
//! A mask is a `--ro-bind <sentinel> <target>` for a directory, or a
//! `--ro-bind /dev/null <file>` for a single file, emitted after every other
//! mount of the plan, so no earlier policy grant or alias bind can expose the
//! target again. A mount works on one path, so a target the child could also
//! reach through a second path would stay readable there. The plan is refused
//! instead of started with an incomplete mask: both the plan's own alias binds
//! and every host mount the read-only bind of `/` carries are checked.

use super::*;

/// Mountinfo of the process compiling the plan: the host view that the
/// recursive `--ro-bind / /` hands to the child.
const MOUNTINFO: &str = "/proc/self/mountinfo";

pub(super) fn append_mask_mounts(
    out: &mut Vec<String>,
    mask: &LinuxBwrapMask,
) -> Result<(), OrbitError> {
    let sentinel = canonical_existing(&mask.sentinel, "sandbox mask sentinel")?;
    if !sentinel.is_dir() {
        return Err(OrbitError::PolicyDenied(format!(
            "sandbox mask sentinel `{}` is not a directory",
            sentinel.display()
        )));
    }
    let mountinfo = std::fs::read_to_string(MOUNTINFO).map_err(|error| {
        OrbitError::Execution(format!(
            "read {MOUNTINFO} to check masked directories for aliases: {error}"
        ))
    })?;
    let mounts = parse_mountinfo(&mountinfo);
    let mut masked_dirs = Vec::new();
    for target in &mask.targets {
        let target = canonical_existing(target, "masked directory")?;
        if !target.is_dir() {
            return Err(OrbitError::PolicyDenied(format!(
                "masked path `{}` is not a directory",
                target.display()
            )));
        }
        if sentinel.starts_with(&target) || target.starts_with(&sentinel) {
            return Err(OrbitError::PolicyDenied(format!(
                "sandbox mask sentinel `{}` overlaps the masked directory `{}`",
                sentinel.display(),
                target.display()
            )));
        }
        let alias = plan_alias(out, &target).or(host_alias(&mounts, &target)?);
        if let Some(alias) = alias {
            return Err(OrbitError::PolicyDenied(format!(
                "linux-bwrap cannot mask `{}`: the sandbox would also reach it at `{}`; \
                 refusing to start with the mask incomplete",
                target.display(),
                alias.display()
            )));
        }
        out.extend([
            "--ro-bind".to_string(),
            sentinel.display().to_string(),
            target.display().to_string(),
        ]);
        masked_dirs.push(target);
    }
    for file in &mask.files {
        let Some(file) = canonical_masked_file(file)? else {
            continue;
        };
        // A file inside a masked directory is already hidden, and the mount
        // point would no longer exist once the sentinel covers its parent.
        if masked_dirs.iter().any(|dir| file.starts_with(dir)) {
            continue;
        }
        let alias = plan_alias(out, &file).or(host_alias(&mounts, &file)?);
        if let Some(alias) = alias {
            return Err(OrbitError::PolicyDenied(format!(
                "linux-bwrap cannot mask `{}`: the sandbox would also reach it at `{}`; \
                 refusing to start with the mask incomplete",
                file.display(),
                alias.display()
            )));
        }
        out.extend([
            "--ro-bind".to_string(),
            "/dev/null".to_string(),
            file.display().to_string(),
        ]);
    }
    Ok(())
}

/// The physical path of a masked file, or `None` when nothing exists there to
/// hide. Anything other than a regular file refuses the plan, because a
/// `/dev/null` bind cannot stand in for it.
fn canonical_masked_file(file: &Path) -> Result<Option<PathBuf>, OrbitError> {
    let canonical = match file.canonicalize() {
        Ok(canonical) => canonical,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(OrbitError::PolicyDenied(format!(
                "linux-bwrap cannot resolve masked file `{}`: {error}",
                file.display()
            )));
        }
    };
    if !canonical.is_file() {
        return Err(OrbitError::PolicyDenied(format!(
            "masked path `{}` is not a regular file",
            canonical.display()
        )));
    }
    Ok(Some(canonical))
}

/// A bind the plan itself makes from one path to another, whose source holds
/// the target or lies inside it: the stable toolchain aliases are the case.
pub(super) fn plan_alias(args: &[String], target: &Path) -> Option<PathBuf> {
    args.windows(3)
        .filter(|triple| matches!(triple[0].as_str(), "--bind" | "--ro-bind"))
        .filter(|triple| triple[1] != triple[2])
        .find_map(|triple| {
            let source = Path::new(&triple[1]);
            let destination = Path::new(&triple[2]);
            if let Ok(rest) = target.strip_prefix(source) {
                Some(destination.join(rest))
            } else if source.starts_with(target) {
                Some(destination.to_path_buf())
            } else {
                None
            }
        })
}

/// One `/proc/self/mountinfo` line, reduced to what an alias check needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MountEntry {
    /// `major:minor` of the filesystem, as mountinfo prints it.
    pub(super) device: String,
    /// The directory inside that filesystem the mount shows.
    pub(super) root: PathBuf,
    /// Where it is mounted.
    pub(super) mount_point: PathBuf,
}

pub(super) fn parse_mountinfo(text: &str) -> Vec<MountEntry> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split(' ');
            let _id = fields.next()?;
            let _parent = fields.next()?;
            let device = fields.next()?.to_string();
            let root = PathBuf::from(unescape_mountinfo(fields.next()?));
            let mount_point = PathBuf::from(unescape_mountinfo(fields.next()?));
            Some(MountEntry {
                device,
                root,
                mount_point,
            })
        })
        .collect()
}

/// Mountinfo writes space, tab, newline and backslash as three-digit octal.
fn unescape_mountinfo(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && index + 3 < bytes.len()
            && bytes[index + 1..=index + 3]
                .iter()
                .all(|digit| (b'0'..=b'7').contains(digit))
        {
            let value = bytes[index + 1..=index + 3]
                .iter()
                .fold(0u32, |value, digit| value * 8 + u32::from(digit - b'0'));
            if let Ok(byte) = u8::try_from(value) {
                out.push(byte);
                index += 4;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Another host mount of the target's filesystem through which the target, or
/// part of it, is reachable at a different path.
///
/// The mount holding the target decides its path inside the filesystem. Every
/// other mount of that filesystem whose root is above that path would show the
/// target again beneath its mount point; one whose root is inside the target
/// shows part of it. Each candidate is confirmed by device and inode, so a
/// mount point shadowed by a later mount is not reported. Mounts beneath the
/// target are hidden by the mask itself.
pub(super) fn host_alias(
    mounts: &[MountEntry],
    target: &Path,
) -> Result<Option<PathBuf>, OrbitError> {
    let Some((holder_index, holder)) = mounts
        .iter()
        .enumerate()
        .filter(|(_, entry)| target.starts_with(&entry.mount_point))
        .max_by_key(|(_, entry)| entry.mount_point.components().count())
    else {
        return Ok(None);
    };
    let Ok(below_holder) = target.strip_prefix(&holder.mount_point) else {
        return Ok(None);
    };
    let inside = holder.root.join(below_holder);
    let target_identity = object_identity(target).ok_or_else(|| {
        OrbitError::Execution(format!(
            "inspect masked directory `{}` for aliases",
            target.display()
        ))
    })?;
    for (index, entry) in mounts.iter().enumerate() {
        if index == holder_index
            || entry.device != holder.device
            || entry.mount_point.starts_with(target)
        {
            continue;
        }
        if let Ok(rest) = inside.strip_prefix(&entry.root) {
            let candidate = entry.mount_point.join(rest);
            if candidate != target && object_identity(&candidate) == Some(target_identity) {
                return Ok(Some(candidate));
            }
        } else if let Ok(rest) = entry.root.strip_prefix(&inside) {
            let exposed = target.join(rest);
            if object_identity(&entry.mount_point).is_some()
                && object_identity(&entry.mount_point) == object_identity(&exposed)
            {
                return Ok(Some(entry.mount_point.clone()));
            }
        }
    }
    Ok(None)
}

#[cfg(unix)]
fn object_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;

    std::fs::metadata(path)
        .ok()
        .map(|metadata| (metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn object_identity(_path: &Path) -> Option<(u64, u64)> {
    None
}
