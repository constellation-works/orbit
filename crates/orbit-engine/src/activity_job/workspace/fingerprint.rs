use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Output;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::executor::automation::vcs::git::git_command;

use super::super::dispatcher::DispatchError;

/// Exact, read-only identity of the Git state that an agent invocation can
/// observe or mutate. Large byte streams are represented by domain-separated
/// SHA-256 identities; untracked files retain one content identity per path so
/// diagnostics can name the primary-checkout delta without staging it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct GitWorktreeFingerprint {
    pub(crate) head: String,
    pub(crate) branch: Option<String>,
    pub(crate) index_sha256: String,
    pub(crate) tracked_patch_sha256: String,
    pub(crate) untracked_content: BTreeMap<String, String>,
    pub(crate) dirty_paths: Vec<String>,
    pub(crate) path_states: BTreeMap<String, GitPathState>,
}

/// Per-path Git identity. Optional identities distinguish an absent index
/// entry, an empty staged/unstaged delta, a deletion from the worktree, and an
/// untracked file without reading file contents into the diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct GitPathState {
    pub(crate) index_entry_sha256: Option<String>,
    pub(crate) staged_patch_sha256: Option<String>,
    pub(crate) worktree_patch_sha256: Option<String>,
    pub(crate) worktree_present: bool,
    pub(crate) untracked_content_sha256: Option<String>,
}

const DIFF_IDENTITY_FLAGS: [&str; 5] = [
    "--binary",
    "--full-index",
    "--no-ext-diff",
    "--no-textconv",
    "--no-renames",
];

pub(crate) fn git_fingerprint(root: &Path) -> Result<GitWorktreeFingerprint, DispatchError> {
    let head = git_stdout(root, &["rev-parse", "--verify", "HEAD"])?;
    let branch_output = git_output_raw(root, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    let branch = branch_output
        .status
        .success()
        .then(|| {
            String::from_utf8_lossy(&branch_output.stdout)
                .trim()
                .to_string()
        })
        .filter(|branch| !branch.is_empty());

    let index = git_stdout_bytes(root, &["ls-files", "--stage", "-z", "--"])?;
    let tracked_patch = git_diff_bytes(root, &["HEAD"])?;
    let status = git_stdout_bytes(
        root,
        &[
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=all",
            "--no-renames",
            "--",
        ],
    )?;
    let (tracked_dirty_paths, untracked_paths) = parse_porcelain_v2(&status)?;
    let untracked_content = untracked_content_identities(root, &untracked_paths)?;

    let mut dirty_paths = tracked_dirty_paths;
    dirty_paths.extend(untracked_content.keys().cloned());
    dirty_paths.sort();
    dirty_paths.dedup();

    let index_entries = index_entries_by_path(&index);
    let has_tracked_dirty = dirty_paths
        .iter()
        .any(|path| !untracked_content.contains_key(path));
    let staged_patches = if has_tracked_dirty {
        split_combined_diff(&git_diff_bytes(root, &["--cached", "HEAD"])?)
    } else {
        BTreeMap::new()
    };
    let worktree_patches = if has_tracked_dirty {
        split_combined_diff(&git_diff_bytes(root, &[])?)
    } else {
        BTreeMap::new()
    };

    let mut path_states = BTreeMap::new();
    for path in &dirty_paths {
        let index_entry = index_entries.get(path).map(Vec::as_slice).unwrap_or(&[]);
        let staged_patch = staged_patches.get(path).map(Vec::as_slice).unwrap_or(&[]);
        let worktree_patch = worktree_patches.get(path).map(Vec::as_slice).unwrap_or(&[]);
        path_states.insert(
            path.clone(),
            GitPathState {
                index_entry_sha256: optional_sha256_identity("git-index-entry-v1", index_entry),
                staged_patch_sha256: optional_sha256_identity(
                    "git-staged-path-patch-v1",
                    staged_patch,
                ),
                worktree_patch_sha256: optional_sha256_identity(
                    "git-worktree-path-patch-v1",
                    worktree_patch,
                ),
                worktree_present: fs::symlink_metadata(root.join(path)).is_ok(),
                untracked_content_sha256: untracked_content.get(path).cloned(),
            },
        );
    }

    Ok(GitWorktreeFingerprint {
        head,
        branch,
        index_sha256: sha256_identity("git-index-v1", &index),
        tracked_patch_sha256: sha256_identity("git-tracked-patch-v1", &tracked_patch),
        untracked_content,
        dirty_paths,
        path_states,
    })
}

pub(crate) fn untracked_file_identity(
    root: &Path,
    path: &str,
) -> Result<Option<String>, DispatchError> {
    let args = ["hash-object", "--no-filters", "--", path];
    let output = git_output_raw(root, &args)?;
    if output.status.success() {
        let identity = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Ok(Some(format!("git-blob:{identity}")));
    }

    match fs::symlink_metadata(root.join(path)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        _ => Err(git_command_error(root, &args, &output)),
    }
}

pub(crate) fn changed_paths(
    root: &Path,
    before: &GitWorktreeFingerprint,
    after: &GitWorktreeFingerprint,
) -> Vec<String> {
    let all_state_paths = before
        .path_states
        .keys()
        .chain(after.path_states.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut paths = BTreeSet::new();
    for path in all_state_paths {
        if before.path_states.get(&path) != after.path_states.get(&path) {
            paths.insert(path);
        }
    }

    if before.head != after.head
        && let Ok(bytes) = git_stdout_bytes(
            root,
            &[
                "diff",
                "--name-only",
                "-z",
                "--no-renames",
                &before.head,
                &after.head,
                "--",
            ],
        )
    {
        paths.extend(nul_paths(&bytes));
    }
    if paths.is_empty() {
        if before.head != after.head {
            paths.insert("<head>".to_string());
        }
        if before.branch != after.branch {
            paths.insert("<branch-ref>".to_string());
        }
        if before.index_sha256 != after.index_sha256 {
            paths.insert("<index>".to_string());
        }
        if before.tracked_patch_sha256 != after.tracked_patch_sha256 {
            paths.insert("<tracked-patch>".to_string());
        }
    }
    paths.into_iter().collect()
}

fn git_diff_bytes(root: &Path, extra: &[&str]) -> Result<Vec<u8>, DispatchError> {
    let mut args = Vec::with_capacity(8 + extra.len());
    args.push("diff");
    args.extend(DIFF_IDENTITY_FLAGS);
    args.extend(extra.iter().copied());
    args.push("--");
    git_stdout_bytes(root, &args)
}

fn parse_porcelain_v2(bytes: &[u8]) -> Result<(Vec<String>, Vec<String>), DispatchError> {
    let mut tracked_dirty = Vec::new();
    let mut untracked = Vec::new();
    let mut records = bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty());
    while let Some(record) = records.next() {
        if record.starts_with(b"#") {
            continue;
        }
        if record.starts_with(b"? ") {
            untracked.push(lossy_path(&record[2..]));
            continue;
        }
        if record.starts_with(b"! ") {
            continue;
        }
        if record.starts_with(b"1 ") {
            tracked_dirty.push(porcelain_path(record, 8)?);
            continue;
        }
        if record.starts_with(b"u ") {
            tracked_dirty.push(porcelain_path(record, 10)?);
            continue;
        }
        if record.starts_with(b"2 ") {
            tracked_dirty.push(porcelain_path(record, 9)?);
            // `-z` rename records are followed by the original path.
            let _orig = records.next();
            continue;
        }
        return Err(DispatchError::CliInvocationPermanent(format!(
            "unrecognized git status --porcelain=v2 record: {}",
            String::from_utf8_lossy(record)
        )));
    }
    Ok((tracked_dirty, untracked))
}

fn porcelain_path(record: &[u8], fields_before_path: usize) -> Result<String, DispatchError> {
    rest_after_n_spaces(record, fields_before_path)
        .map(lossy_path)
        .ok_or_else(|| {
            DispatchError::CliInvocationPermanent(format!(
                "git status --porcelain=v2 record missing path: {}",
                String::from_utf8_lossy(record)
            ))
        })
}

fn rest_after_n_spaces(bytes: &[u8], n: usize) -> Option<&[u8]> {
    let mut seen = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b' ' {
            seen += 1;
            if seen == n {
                return Some(&bytes[index + 1..]);
            }
        }
    }
    None
}

fn lossy_path(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn index_entries_by_path(index: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut entries = BTreeMap::new();
    for record in index
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
            continue;
        };
        let path = lossy_path(&record[tab + 1..]);
        let entry: &mut Vec<u8> = entries.entry(path).or_default();
        entry.extend_from_slice(record);
        entry.push(0);
    }
    entries
}

fn split_combined_diff(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut patches = BTreeMap::new();
    for chunk in diff_chunks(bytes) {
        if let Some(path) = path_from_diff_chunk(chunk) {
            patches.insert(path, chunk.to_vec());
        }
    }
    patches
}

fn diff_chunks(bytes: &[u8]) -> Vec<&[u8]> {
    let starts = diff_chunk_starts(bytes);
    if starts.is_empty() {
        return Vec::new();
    }
    let mut chunks = Vec::with_capacity(starts.len());
    for (index, start) in starts.iter().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(bytes.len());
        chunks.push(&bytes[*start..end]);
    }
    chunks
}

fn diff_chunk_starts(bytes: &[u8]) -> Vec<usize> {
    const MARKERS: [&[u8]; 3] = [b"diff --git ", b"diff --cc ", b"diff --combined "];
    let mut starts = Vec::new();
    let mut search_from = 0;
    while search_from < bytes.len() {
        let rest = &bytes[search_from..];
        let next = MARKERS
            .iter()
            .filter_map(|marker| {
                rest.windows(marker.len())
                    .position(|window| window == *marker)
            })
            .min();
        let Some(relative) = next else {
            break;
        };
        let absolute = search_from + relative;
        if absolute == 0 || bytes[absolute - 1] == b'\n' {
            starts.push(absolute);
        }
        search_from = absolute + 1;
    }
    starts
}

fn path_from_diff_chunk(chunk: &[u8]) -> Option<String> {
    let line_end = chunk
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap_or(chunk.len());
    let line = String::from_utf8_lossy(&chunk[..line_end]);
    let line = line.trim_end_matches('\r');
    if let Some(rest) = line.strip_prefix("diff --git ") {
        return parse_diff_git_paths(rest);
    }
    if let Some(rest) = line.strip_prefix("diff --cc ") {
        return parse_single_diff_path(rest);
    }
    if let Some(rest) = line.strip_prefix("diff --combined ") {
        return parse_single_diff_path(rest);
    }
    None
}

fn parse_diff_git_paths(rest: &str) -> Option<String> {
    if rest.starts_with('"') {
        let (first, rest) = unescape_git_c_quoted(rest)?;
        let rest = rest.trim_start();
        let (second, _) = unescape_git_c_quoted(rest)?;
        return strip_diff_prefix(&second).or_else(|| strip_diff_prefix(&first));
    }

    let rest = rest.strip_prefix("a/")?;
    if rest.len() >= 3 && rest.len() % 2 == 1 {
        let path_len = (rest.len() - 3) / 2;
        if rest.get(path_len..path_len + 3) == Some(" b/")
            && rest.get(..path_len) == rest.get(path_len + 3..)
        {
            return Some(rest[..path_len].to_string());
        }
    }
    rest.split_once(" b/").map(|(_, right)| right.to_string())
}

fn parse_single_diff_path(rest: &str) -> Option<String> {
    let rest = rest.trim();
    if rest.starts_with('"') {
        let (quoted, _) = unescape_git_c_quoted(rest)?;
        return Some(strip_diff_prefix(&quoted).unwrap_or(quoted));
    }
    Some(rest.to_string())
}

fn strip_diff_prefix(path: &str) -> Option<String> {
    path.strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .map(ToOwned::to_owned)
}

fn unescape_git_c_quoted(input: &str) -> Option<(String, &str)> {
    let input = input.strip_prefix('"')?;
    let mut decoded = String::new();
    let mut bytes = input.as_bytes();
    while let Some((head, rest)) = bytes.split_first() {
        match *head {
            b'"' => {
                return Some((decoded, std::str::from_utf8(rest).ok()?));
            }
            b'\\' => {
                let (escaped, remaining) = unescape_git_escape(rest)?;
                decoded.push(escaped);
                bytes = remaining;
            }
            byte => {
                decoded.push(char::from(byte));
                bytes = rest;
            }
        }
    }
    None
}

fn unescape_git_escape(bytes: &[u8]) -> Option<(char, &[u8])> {
    let (head, rest) = bytes.split_first()?;
    match *head {
        b'n' => Some(('\n', rest)),
        b't' => Some(('\t', rest)),
        b'r' => Some(('\r', rest)),
        b'a' => Some(('\u{0007}', rest)),
        b'b' => Some(('\u{0008}', rest)),
        b'f' => Some(('\u{000c}', rest)),
        b'v' => Some(('\u{000b}', rest)),
        b'\\' => Some(('\\', rest)),
        b'"' => Some(('"', rest)),
        b'0'..=b'7' => {
            if bytes.len() < 3 {
                return None;
            }
            let octal = std::str::from_utf8(&bytes[..3]).ok()?;
            let value = u8::from_str_radix(octal, 8).ok()?;
            Some((char::from(value), &bytes[3..]))
        }
        byte => Some((char::from(byte), rest)),
    }
}

fn untracked_content_identities(
    root: &Path,
    paths: &[String],
) -> Result<BTreeMap<String, String>, DispatchError> {
    let mut identities = BTreeMap::new();
    let mut batch_paths = Vec::new();
    for path in paths {
        if path.contains('\n') {
            if let Some(identity) = untracked_file_identity(root, path)? {
                identities.insert(path.clone(), identity);
            }
        } else {
            batch_paths.push(path.clone());
        }
    }

    let mut remaining = batch_paths;
    for _ in 0..3 {
        if remaining.is_empty() {
            break;
        }
        let args = ["hash-object", "--no-filters", "--stdin-paths"];
        let stdin = remaining.join("\n");
        let stdin = format!("{stdin}\n");
        let output = git_output_with_stdin(root, &args, stdin.as_bytes())?;
        if output.status.success() {
            let hashes = String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            if hashes.len() != remaining.len() {
                return Err(DispatchError::CliInvocationPermanent(format!(
                    "snapshot Git state in '{}' with `git hash-object --stdin-paths` returned {} hashes for {} paths",
                    root.display(),
                    hashes.len(),
                    remaining.len()
                )));
            }
            for (path, hash) in remaining.iter().zip(hashes) {
                identities.insert(path.clone(), format!("git-blob:{hash}"));
            }
            break;
        }

        let existed = remaining
            .iter()
            .filter(|path| fs::symlink_metadata(root.join(path)).is_ok())
            .cloned()
            .collect::<Vec<_>>();
        // An atomic tracked-file replacement briefly exposes an untracked
        // sibling temp file. It may disappear between status and hash-object;
        // omit it instead of turning an unrelated snapshot into a failure.
        if existed.len() == remaining.len() {
            return Err(git_command_error(root, &args, &output));
        }
        remaining = existed;
        if remaining.is_empty() {
            break;
        }
    }
    Ok(identities)
}

pub(crate) fn nul_paths(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect()
}

fn sha256_identity(domain: &str, bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

fn optional_sha256_identity(domain: &str, bytes: &[u8]) -> Option<String> {
    (!bytes.is_empty()).then(|| sha256_identity(domain, bytes))
}

pub(crate) fn git_stdout(root: &Path, args: &[&str]) -> Result<String, DispatchError> {
    let bytes = git_stdout_bytes(root, args)?;
    Ok(String::from_utf8_lossy(&bytes).trim().to_string())
}

pub(crate) fn git_stdout_bytes(root: &Path, args: &[&str]) -> Result<Vec<u8>, DispatchError> {
    let output = git_output_raw(root, args)?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    Err(git_command_error(root, args, &output))
}

pub(crate) fn git_output_raw(root: &Path, args: &[&str]) -> Result<Output, DispatchError> {
    git_command(root, args).output().map_err(|error| {
        DispatchError::CliInvocationPermanent(format!(
            "snapshot Git state in '{}': {error}",
            root.display()
        ))
    })
}

fn git_output_with_stdin(
    root: &Path,
    args: &[&str],
    stdin_bytes: &[u8],
) -> Result<Output, DispatchError> {
    let io_error = |error: std::io::Error| {
        DispatchError::CliInvocationPermanent(format!(
            "snapshot Git state in '{}': {error}",
            root.display()
        ))
    };
    // Feed stdin from a file, not a pipe we still own: `hash-object --stdin-paths`
    // waits for EOF, and `Child::wait_with_output` waits for the child, so a
    // parent-written pipe deadlocks.
    let mut temp = tempfile::Builder::new()
        .prefix("orbit-git-stdin-")
        .tempfile()
        .map_err(io_error)?;
    temp.write_all(stdin_bytes).map_err(io_error)?;
    temp.flush().map_err(io_error)?;
    let stdin = fs::File::open(temp.path()).map_err(io_error)?;
    let mut command = git_command(root, args);
    command.stdin(stdin);
    command.output().map_err(io_error)
}

pub(crate) fn git_command_error(root: &Path, args: &[&str], output: &Output) -> DispatchError {
    let stderr = String::from_utf8_lossy(&output.stderr);
    DispatchError::CliInvocationPermanent(format!(
        "snapshot Git state in '{}' with `git {}` failed (status {}): {}",
        root.display(),
        args.join(" "),
        output.status,
        stderr.trim()
    ))
}
