//! The managed worktree a resident cycle's agent launches in.
//!
//! The cycle is a reviewer-profile activity that acts through Orbit tools,
//! never through files, so it used to launch in the registered checkout.
//! On Linux that direct launch cannot enforce a non-subtree `denyModify`
//! such as `**/.env`: Bubblewrap can only mount the matches that exist at
//! spawn, and a post-run guard is what catches a match created afterwards.
//! The sandbox only carries that guard for a managed worktree, so a direct
//! resident launch was refused before the agent ran [ORB-24301].
//!
//! `resident_observe` therefore creates a detached worktree of the
//! workspace `HEAD` under `.orbit/state/worktrees/resident-<run_id>` — the
//! root sandbox resolution already recognizes as a managed worktree — and
//! the brief names it as the cycle's `workspace_path`. `resident_finalize`
//! removes it when Git accepts the removal. A registered worktree Git
//! refuses (a lock, or any other refusal) stays in place and the refusal is
//! reported. A directory that is not a registered worktree is removed only
//! when this run's owner record proves the run created it [ORB-24317]; the
//! directory name is not that proof. Nothing the agent writes in the
//! worktree is committed or delivered.

use std::io::Read;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::git::{GIT_CHECKOUT_TIMEOUT, GitCommandOutput, run_git, run_git_within};

use crate::OrbitRuntime;

const OWNER_RECORD_VERSION: &str = "resident-cycle-worktree-owner/v1";
const OWNER_RECORD_DIR: &str = "resident-cycle-worktree-owners";
const OWNER_NONCE_FILE: &str = ".resident-cycle-owner";
const OWNER_RECORD_MAX_BYTES: usize = 1024;
const OWNER_NONCE_HEX_LEN: usize = 64;

/// The managed worktree for `run_id`'s cycle, or `None` when the run id
/// cannot name a single directory component.
pub(crate) fn resident_cycle_worktree_path(worktrees_dir: &Path, run_id: &str) -> Option<PathBuf> {
    if run_id.is_empty()
        || !run_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    Some(worktrees_dir.join(format!("resident-{run_id}")))
}

impl OrbitRuntime {
    /// Create (or reuse, for a resumed run) this cycle's managed worktree
    /// at the workspace `HEAD`.
    ///
    /// Best-effort: a workspace whose `HEAD` cannot be checked out (not a
    /// Git repository, no commit yet) yields `None` and the cycle launches
    /// as it did before, in the registered checkout. On a Git workspace —
    /// the launch that was refused — the worktree is created and the
    /// sandbox marks the launch managed. The error string is the reason a
    /// worktree was not created, including a preserved pre-existing directory.
    pub(crate) fn create_resident_cycle_worktree(
        &self,
        run_id: &str,
    ) -> (Option<PathBuf>, Option<String>) {
        match self.create_resident_cycle_worktree_inner(run_id) {
            Ok(path) => (Some(path), None),
            Err(error) => {
                tracing::warn!(
                    run_id,
                    %error,
                    "resident cycle launches without a managed worktree"
                );
                (None, Some(error.to_string()))
            }
        }
    }

    fn create_resident_cycle_worktree_inner(&self, run_id: &str) -> Result<PathBuf, OrbitError> {
        let path =
            resident_cycle_worktree_path(&self.paths().worktrees_dir, run_id).ok_or_else(|| {
                OrbitError::InvalidInput(format!("run id '{run_id}' cannot name a worktree"))
            })?;
        refuse_if_symlink(&path)?;
        // A resumed run re-enters its live cycle: keep the worktree it
        // already holds, scratch included, rather than replacing it.
        if path.join(".git").exists() {
            return path
                .canonicalize()
                .map_err(|error| OrbitError::Execution(format!("resolve worktree: {error}")));
        }
        let head = run_git(&self.paths().repo_root, &["rev-parse", "HEAD"])
            .map_err(|error| OrbitError::Execution(error.to_string()))?;
        if !head.success {
            return Err(OrbitError::Execution(format!(
                "resolve workspace HEAD for the cycle worktree: {}",
                head.stderr.trim()
            )));
        }
        let head = head.stdout.trim();
        self.remove_resident_cycle_worktree(run_id)?;
        if worktree_path_kind(&path)? != WorktreePathKind::Absent {
            return Err(preserved(
                &path_text(&path),
                "the path was still present after cleanup",
            ));
        }
        if let Some(parent) = path.parent() {
            orbit_common::fs::io::create_private_dir_all(parent).map_err(|error| {
                OrbitError::Execution(format!(
                    "create worktree directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
        match orbit_common::fs::io::create_private_dir(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(preserved(
                    &path_text(&path),
                    "it appeared before this run could create it",
                ));
            }
            Err(error) => {
                return Err(OrbitError::Execution(format!(
                    "create cycle worktree directory {}: {error}",
                    path.display()
                )));
            }
        }
        if let Err(error) = record_created_directory(&self.paths().state_dir, run_id, &path) {
            rollback_empty_created_directory(&path);
            return Err(error);
        }
        let target = utf8_path(&path)?;
        let output = run_git_within(
            &self.paths().repo_root,
            &["worktree", "add", "--detach", "--quiet", &target, head],
            GIT_CHECKOUT_TIMEOUT,
        )
        .map_err(|error| OrbitError::Execution(error.to_string()))?;
        if !output.success {
            return Err(OrbitError::Execution(format!(
                "create cycle worktree {target} at {head}: {}",
                output.stderr.trim()
            )));
        }
        record_checkout_owner(&self.paths().state_dir, run_id, &path)?;
        path.canonicalize()
            .map_err(|error| OrbitError::Execution(format!("resolve worktree {target}: {error}")))
    }

    /// Remove `run_id`'s cycle worktree, if it exists and this run may.
    ///
    /// A registered worktree is removed only by a successful
    /// `git worktree remove --force`. Git's refusal leaves the directory and
    /// its registration untouched. An unregistered directory is removed only
    /// with the owner record written when this run created it.
    pub(crate) fn remove_resident_cycle_worktree(&self, run_id: &str) -> Result<(), OrbitError> {
        let Some(path) = resident_cycle_worktree_path(&self.paths().worktrees_dir, run_id) else {
            return Ok(());
        };
        let target = path_text(&path);
        match worktree_path_kind(&path)? {
            WorktreePathKind::Absent => {
                return clear_owner_record(&self.paths().state_dir, run_id);
            }
            WorktreePathKind::NotDirectory => {
                return Err(preserved(&target, "it is not a directory this run created"));
            }
            WorktreePathKind::Directory => {}
        }
        let state_dir = &self.paths().state_dir;
        let repo_root = &self.paths().repo_root;
        if worktree_is_registered(repo_root, &path)? {
            self.remove_registered_worktree(run_id, &path, &target)
        } else {
            remove_if_owned(repo_root, state_dir, run_id, &path, &target, None)
        }
    }

    fn remove_registered_worktree(
        &self,
        run_id: &str,
        path: &Path,
        target: &str,
    ) -> Result<(), OrbitError> {
        let path_arg = utf8_path(path)?;
        let output = run_git_within(
            &self.paths().repo_root,
            &["worktree", "remove", "--force", &path_arg],
            GIT_CHECKOUT_TIMEOUT,
        )
        .map_err(|error| OrbitError::Execution(error.to_string()))?;
        if output.success {
            return clear_owner_record(&self.paths().state_dir, run_id);
        }
        let detail = git_failure_detail(&output);
        match worktree_is_registered(&self.paths().repo_root, path) {
            Ok(true) => Err(OrbitError::Execution(format!(
                "refused to remove registered resident cycle worktree {target}: {detail}"
            ))),
            Err(error) => Err(preserved(
                target,
                &format!(
                    "git worktree remove failed ({detail}) and registration could not be rechecked: {error}"
                ),
            )),
            Ok(false) => remove_if_owned(
                &self.paths().repo_root,
                &self.paths().state_dir,
                run_id,
                path,
                target,
                Some(detail.as_str()),
            ),
        }
    }

    /// Best-effort removal for a superseded cycle: a leftover costs disk,
    /// never correctness, so a failure is recorded instead of raised.
    ///
    /// The returned string is the refusal, when the worktree was kept.
    pub(crate) fn discard_resident_cycle_worktree(&self, run_id: &str) -> Option<String> {
        match self.remove_resident_cycle_worktree(run_id) {
            Ok(()) => None,
            Err(error) => {
                tracing::warn!(run_id, %error, "resident cycle worktree removal failed");
                Some(error.to_string())
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorktreePathKind {
    Absent,
    Directory,
    NotDirectory,
}

fn worktree_path_kind(path: &Path) -> Result<WorktreePathKind, OrbitError> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(WorktreePathKind::Absent),
        Err(error) => Err(preserved(
            &path_text(path),
            &format!("could not inspect it: {error}"),
        )),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Ok(WorktreePathKind::NotDirectory)
        }
        Ok(_) => Ok(WorktreePathKind::Directory),
    }
}

fn refuse_if_symlink(path: &Path) -> Result<(), OrbitError> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(preserved(
            &path_text(path),
            &format!("could not inspect it: {error}"),
        )),
        Ok(metadata) if metadata.file_type().is_symlink() => Err(preserved(
            &path_text(path),
            "it is a symlink, not a directory this run created",
        )),
        Ok(_) => Ok(()),
    }
}

fn remove_if_owned(
    repo_root: &Path,
    state_dir: &Path,
    run_id: &str,
    path: &Path,
    target: &str,
    git_detail: Option<&str>,
) -> Result<(), OrbitError> {
    let denied = match ownership(state_dir, run_id, path) {
        Ownership::Proven => None,
        Ownership::Denied(reason) => Some(reason),
    };
    if let Some(reason) = denied {
        let reason = match git_detail {
            Some(detail) => format!("git worktree remove failed ({detail}) and {reason}"),
            None => reason,
        };
        return Err(preserved(target, &reason));
    }
    // A registration that appeared after the first check is Git's to keep.
    if worktree_is_registered(repo_root, path)? {
        return Err(preserved(
            target,
            "it became a registered worktree before fallback deletion",
        ));
    }
    if !matches!(ownership(state_dir, run_id, path), Ownership::Proven) {
        return Err(preserved(
            target,
            "ownership evidence changed before deletion",
        ));
    }
    std::fs::remove_dir_all(path).map_err(|error| {
        OrbitError::Execution(format!("remove cycle worktree {target}: {error}"))
    })?;
    clear_owner_record(state_dir, run_id)?;
    warn_if_prune_fails(repo_root, target);
    Ok(())
}

fn warn_if_prune_fails(repo_root: &Path, target: &str) {
    match run_git(repo_root, &["worktree", "prune"]) {
        Ok(output) if output.success => {}
        Ok(output) => {
            tracing::warn!(
                target,
                stderr = %output.stderr.trim(),
                "resident cycle worktree prune failed after owned removal"
            );
        }
        Err(error) => {
            tracing::warn!(
                target,
                %error,
                "resident cycle worktree prune failed after owned removal"
            );
        }
    }
}

fn worktree_is_registered(repo_root: &Path, path: &Path) -> Result<bool, OrbitError> {
    let output = run_git(repo_root, &["worktree", "list", "--porcelain"])
        .map_err(|error| OrbitError::Execution(error.to_string()))?;
    if !output.success {
        return Err(preserved(
            &path_text(path),
            &format!(
                "could not list registered worktrees: {}",
                git_failure_detail(&output)
            ),
        ));
    }
    porcelain_lists(&output.stdout, path)
}

fn porcelain_lists(porcelain: &str, path: &Path) -> Result<bool, OrbitError> {
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        preserved(
            &path_text(path),
            &format!("could not resolve it to compare with git's worktree list: {error}"),
        )
    })?;
    for line in porcelain.lines() {
        let Some(recorded) = line.strip_prefix("worktree ") else {
            continue;
        };
        if recorded.is_empty() {
            continue;
        }
        let recorded_path = Path::new(recorded);
        if recorded_path == path || recorded_path == canonical {
            return Ok(true);
        }
        if std::fs::canonicalize(recorded_path).ok().as_ref() == Some(&canonical) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn git_failure_detail(output: &GitCommandOutput) -> String {
    let detail = output.stderr.trim();
    if !detail.is_empty() {
        return detail.to_string();
    }
    match output.code {
        Some(code) => format!("git exited {code} with no stderr"),
        None => "git was terminated by a signal".to_string(),
    }
}

fn preserved(target: &str, reason: &str) -> OrbitError {
    OrbitError::Execution(format!(
        "preserved resident cycle worktree path {target}: {reason}"
    ))
}

fn path_text(path: &Path) -> String {
    path.display().to_string()
}

fn utf8_path(path: &Path) -> Result<String, OrbitError> {
    path.to_str().map(str::to_string).ok_or_else(|| {
        preserved(
            &path_text(path),
            "its path is not Unicode, so git cannot be asked about this exact path",
        )
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DirectoryIdentity {
    dev: u64,
    ino: u64,
}

fn directory_identity(path: &Path) -> Result<Option<DirectoryIdentity>, std::io::Error> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Ok(None);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(Some(DirectoryIdentity {
            dev: metadata.dev(),
            ino: metadata.ino(),
        }))
    }
    #[cfg(not(unix))]
    {
        Ok(None)
    }
}

fn real_directory(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OwnerRecord {
    created: Option<DirectoryIdentity>,
    checkout: Option<DirectoryIdentity>,
    nonce_sha256: Option<String>,
}

enum Ownership {
    Proven,
    Denied(String),
}

fn ownership(state_dir: &Path, run_id: &str, path: &Path) -> Ownership {
    let record_path = owner_record_path(state_dir, run_id);
    match std::fs::symlink_metadata(&record_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ownership::Denied("no owner record for this directory".to_string());
        }
        Err(error) => {
            return Ownership::Denied(format!("owner record could not be inspected: {error}"));
        }
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Ownership::Denied("owner record is not a regular file".to_string());
        }
        Ok(_) => {}
    }
    let Some(bytes) = read_exact_limited(&record_path, OWNER_RECORD_MAX_BYTES) else {
        return Ownership::Denied("owner record could not be read".to_string());
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ownership::Denied("owner record is not utf-8".to_string());
    };
    let Some(record) = parse_owner_record(text) else {
        return Ownership::Denied(
            "owner record is not proof this run created the directory".to_string(),
        );
    };
    if record_matches(&record, path) {
        Ownership::Proven
    } else {
        Ownership::Denied("owner record does not match the directory".to_string())
    }
}

fn record_matches(record: &OwnerRecord, path: &Path) -> bool {
    let Ok(identity) = directory_identity(path) else {
        return false;
    };
    if record.checkout.is_some() || record.nonce_sha256.is_some() {
        let checkout_ok = match record.checkout {
            Some(expected) => identity == Some(expected),
            None => identity.is_none() && real_directory(path),
        };
        return checkout_ok && nonce_matches(path, record.nonce_sha256.as_deref());
    }
    match record.created {
        Some(expected) => identity == Some(expected),
        None => false,
    }
}

fn nonce_matches(worktree: &Path, expected: Option<&str>) -> bool {
    let Some(expected) = expected else {
        return false;
    };
    let orbit = worktree.join(".orbit");
    if !real_directory(&orbit) {
        return false;
    }
    let Some(bytes) = read_exact_limited(&orbit.join(OWNER_NONCE_FILE), OWNER_NONCE_HEX_LEN) else {
        return false;
    };
    if bytes.len() != OWNER_NONCE_HEX_LEN {
        return false;
    }
    blake3::hash(&bytes).to_hex().to_string() == expected
}

fn record_created_directory(state_dir: &Path, run_id: &str, path: &Path) -> Result<(), OrbitError> {
    let Some(created) = directory_identity(path).map_err(|error| {
        OrbitError::Execution(format!(
            "inspect new cycle worktree {}: {error}",
            path.display()
        ))
    })?
    else {
        // No stable directory identity on this platform. Fallback deletion
        // stays unavailable until a checkout nonce is recorded; the empty
        // directory this attempt just created is not named as owned.
        return Ok(());
    };
    write_owner_record(
        state_dir,
        run_id,
        &OwnerRecord {
            created: Some(created),
            checkout: None,
            nonce_sha256: None,
        },
    )
}

fn record_checkout_owner(state_dir: &Path, run_id: &str, path: &Path) -> Result<(), OrbitError> {
    let denied_root = path.join(".orbit");
    match std::fs::symlink_metadata(&denied_root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            orbit_common::fs::io::create_private_dir(&denied_root).map_err(|error| {
                OrbitError::Execution(format!(
                    "prepare cycle worktree deny root {}: {error}",
                    denied_root.display()
                ))
            })?;
        }
        Err(error) => {
            return Err(OrbitError::Execution(format!(
                "prepare cycle worktree deny root {}: {error}",
                denied_root.display()
            )));
        }
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(OrbitError::Execution(format!(
                "prepare cycle worktree deny root {}: not a directory",
                denied_root.display()
            )));
        }
    }
    let nonce = random_nonce_hex()?;
    let nonce_path = denied_root.join(OWNER_NONCE_FILE);
    orbit_common::fs::io::write_new_private_text(&nonce_path, &nonce).map_err(|error| {
        OrbitError::Execution(format!(
            "record cycle worktree owner nonce {}: {error}",
            nonce_path.display()
        ))
    })?;
    let read_back = read_exact_limited(&nonce_path, OWNER_NONCE_HEX_LEN).ok_or_else(|| {
        OrbitError::Execution(format!(
            "read cycle worktree owner nonce {}",
            nonce_path.display()
        ))
    })?;
    if read_back.as_slice() != nonce.as_bytes() {
        return Err(OrbitError::Execution(format!(
            "cycle worktree owner nonce {} did not match what this run wrote",
            nonce_path.display()
        )));
    }
    let identity = directory_identity(path).map_err(|error| {
        OrbitError::Execution(format!(
            "inspect cycle worktree {}: {error}",
            path.display()
        ))
    })?;
    let created = match read_record_file(&owner_record_path(state_dir, run_id)) {
        Some(record) => record.created,
        None => None,
    };
    write_owner_record(
        state_dir,
        run_id,
        &OwnerRecord {
            created,
            checkout: identity,
            nonce_sha256: Some(blake3::hash(&read_back).to_hex().to_string()),
        },
    )
}

fn rollback_empty_created_directory(path: &Path) {
    let Ok(Some(identity)) = directory_identity(path) else {
        return;
    };
    if !directory_is_empty(path) {
        return;
    }
    // `remove_dir` fails when the directory gained contents after the
    // emptiness check, so a raced writer is left in place.
    if directory_identity(path).ok().flatten() != Some(identity) {
        return;
    }
    let _ = std::fs::remove_dir(path);
}

fn directory_is_empty(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_none())
}

fn owner_record_path(state_dir: &Path, run_id: &str) -> PathBuf {
    state_dir.join(OWNER_RECORD_DIR).join(run_id)
}

fn write_owner_record(
    state_dir: &Path,
    run_id: &str,
    record: &OwnerRecord,
) -> Result<(), OrbitError> {
    let path = owner_record_path(state_dir, run_id);
    orbit_common::fs::io::atomic_write_text(&path, &render_owner_record(record)).map_err(|error| {
        OrbitError::Execution(format!(
            "record resident cycle worktree owner {}: {error}",
            path.display()
        ))
    })
}

fn clear_owner_record(state_dir: &Path, run_id: &str) -> Result<(), OrbitError> {
    let path = owner_record_path(state_dir, run_id);
    orbit_common::fs::io::remove_path_if_exists(&path).map_err(|error| {
        OrbitError::Execution(format!(
            "clear resident cycle worktree owner record {}: {error}",
            path.display()
        ))
    })
}

fn read_record_file(path: &Path) -> Option<OwnerRecord> {
    let bytes = read_exact_limited(path, OWNER_RECORD_MAX_BYTES)?;
    let text = std::str::from_utf8(&bytes).ok()?;
    parse_owner_record(text)
}

fn render_owner_record(record: &OwnerRecord) -> String {
    let mut lines = vec![OWNER_RECORD_VERSION.to_string()];
    if let Some(created) = record.created {
        lines.push(format!("created_dev={}", created.dev));
        lines.push(format!("created_ino={}", created.ino));
    }
    if let Some(checkout) = record.checkout {
        lines.push(format!("checkout_dev={}", checkout.dev));
        lines.push(format!("checkout_ino={}", checkout.ino));
    }
    if let Some(nonce) = &record.nonce_sha256 {
        lines.push(format!("nonce_sha256={nonce}"));
    }
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

fn parse_owner_record(text: &str) -> Option<OwnerRecord> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last().copied() != Some("") {
        return None;
    }
    lines.pop();
    if lines.first().copied() != Some(OWNER_RECORD_VERSION) {
        return None;
    }
    let mut created_dev = None;
    let mut created_ino = None;
    let mut checkout_dev = None;
    let mut checkout_ino = None;
    let mut nonce_sha256 = None;
    for line in lines.into_iter().skip(1) {
        if line.is_empty() {
            return None;
        }
        let (key, value) = line.split_once('=')?;
        if value.is_empty() {
            return None;
        }
        match key {
            "created_dev" => assign_u64(&mut created_dev, value)?,
            "created_ino" => assign_u64(&mut created_ino, value)?,
            "checkout_dev" => assign_u64(&mut checkout_dev, value)?,
            "checkout_ino" => assign_u64(&mut checkout_ino, value)?,
            "nonce_sha256" => {
                if nonce_sha256.is_some() || !is_lowercase_hex(value, OWNER_NONCE_HEX_LEN) {
                    return None;
                }
                nonce_sha256 = Some(value.to_string());
            }
            _ => return None,
        }
    }
    let created = pair_identity(created_dev, created_ino)?;
    let checkout = pair_identity(checkout_dev, checkout_ino)?;
    if checkout.is_some() && nonce_sha256.is_none() {
        return None;
    }
    if checkout.is_none() && nonce_sha256.is_some() && created.is_some() {
        return None;
    }
    if created.is_none() && checkout.is_none() && nonce_sha256.is_none() {
        return None;
    }
    Some(OwnerRecord {
        created,
        checkout,
        nonce_sha256,
    })
}

fn assign_u64(slot: &mut Option<u64>, value: &str) -> Option<()> {
    if slot.is_some() {
        return None;
    }
    *slot = Some(value.parse().ok()?);
    Some(())
}

fn pair_identity(dev: Option<u64>, ino: Option<u64>) -> Option<Option<DirectoryIdentity>> {
    match (dev, ino) {
        (None, None) => Some(None),
        (Some(dev), Some(ino)) => Some(Some(DirectoryIdentity { dev, ino })),
        _ => None,
    }
}

fn is_lowercase_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn random_nonce_hex() -> Result<String, OrbitError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| {
        OrbitError::Execution(format!("resident cycle worktree owner nonce: {error}"))
    })?;
    Ok(hex_encode(&bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

fn read_exact_limited(path: &Path, max: usize) -> Option<Vec<u8>> {
    let file = orbit_common::fs::io::open_read_only_no_follow(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > max as u64 {
        return None;
    }
    let mut buf = Vec::new();
    file.take(u64::try_from(max).ok()?.saturating_add(1))
        .read_to_end(&mut buf)
        .ok()?;
    if buf.len() > max {
        return None;
    }
    Some(buf)
}
