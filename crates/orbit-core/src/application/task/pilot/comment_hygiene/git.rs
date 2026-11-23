//! Bounded, streaming Git tree/history/blob readers for `comment-hygiene`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use orbit_common::fs::git::{git_command, run_git};
use orbit_common::process::supervisor::{
    Periodic, ProcessStopCause, Stdin, Stream, SupervisedChild, Supervision, Termination,
};

use super::is_iso_date;

/// Deadline for one local streaming Git history or tree scan.
const GIT_SCAN_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum stdout bytes a streaming Git listing/log scan may write to its temp file.
const GIT_LISTING_LIMIT: u64 = 256 * 1024 * 1024;
/// How often supervision checks the temp-file size limit.
const GIT_LIMIT_POLL: Duration = Duration::from_millis(20);

pub(super) fn list_tracked_blobs(
    workspace_root: &Path,
    revision: &str,
    excluded_by_rule: &mut BTreeMap<String, usize>,
) -> Result<BTreeMap<String, String>, String> {
    let mut file = run_git_captured(
        workspace_root,
        &["ls-tree", "-r", "-z", revision],
        None,
        GIT_LISTING_LIMIT,
    )?;
    let mut reader = BufReader::new(&mut file);
    let mut buf = Vec::new();
    let mut blobs = BTreeMap::new();
    loop {
        buf.clear();
        let read = reader
            .read_until(0, &mut buf)
            .map_err(|error| format!("read git ls-tree output: {error}"))?;
        if read == 0 {
            break;
        }
        if buf.last() == Some(&0) {
            buf.pop();
        }
        if buf.is_empty() {
            continue;
        }
        let Some(tab) = buf.iter().position(|&byte| byte == b'\t') else {
            return Err("malformed git ls-tree entry (missing tab)".to_string());
        };
        let header = std::str::from_utf8(&buf[..tab])
            .map_err(|error| format!("invalid UTF-8 in git ls-tree header: {error}"))?;
        let path = std::str::from_utf8(&buf[tab + 1..])
            .map_err(|error| format!("non-UTF-8 tracked path in git ls-tree: {error}"))?;
        let mut parts = header.split_ascii_whitespace();
        let (Some(mode), Some(kind), Some(oid)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(format!("malformed git ls-tree header {header:?}"));
        };
        if kind == "blob" && matches!(mode, "100644" | "100755") {
            blobs.insert(path.to_string(), oid.to_string());
        } else {
            *excluded_by_rule
                .entry("non_regular_file".to_string())
                .or_insert(0) += 1;
        }
    }
    Ok(blobs)
}

pub(super) fn commit_exists(workspace_root: &Path, sha: &str) -> Result<bool, String> {
    let spec = format!("{sha}^{{commit}}");
    let output = run_git(workspace_root, &["cat-file", "-e", &spec])
        .map_err(|error| format!("check commit {sha}: {error}"))?;
    Ok(output.success)
}

pub(super) fn paths_touched_between(
    workspace_root: &Path,
    checked_through: &str,
    revision: &str,
) -> Result<BTreeSet<String>, String> {
    let range = format!("{checked_through}..{revision}");
    let mut file = run_git_captured(
        workspace_root,
        &["log", "--name-only", "-z", "--format=", &range],
        None,
        GIT_LISTING_LIMIT,
    )?;
    let mut reader = BufReader::new(&mut file);
    let mut buf = Vec::new();
    let mut touched = BTreeSet::new();
    loop {
        buf.clear();
        let read = reader
            .read_until(0, &mut buf)
            .map_err(|error| format!("read git log output for {range}: {error}"))?;
        if read == 0 {
            break;
        }
        if buf.last() == Some(&0) {
            buf.pop();
        }
        let mut slice = buf.as_slice();
        while let Some(rest) = slice.strip_prefix(b"\n") {
            slice = rest;
        }
        if slice.is_empty() {
            continue;
        }
        if let Ok(path) = std::str::from_utf8(slice) {
            touched.insert(path.to_string());
        }
    }
    Ok(touched)
}

pub(super) fn latest_commit_dates(
    workspace_root: &Path,
    revision: &str,
    needed: &BTreeSet<String>,
) -> Result<BTreeMap<String, String>, String> {
    let mut remaining = needed.clone();
    let mut dates = BTreeMap::new();
    let mut file = run_git_captured(
        workspace_root,
        &["log", "--format=%x1e%cI", "--name-only", "-z", revision],
        None,
        GIT_LISTING_LIMIT,
    )?;
    let mut reader = BufReader::new(&mut file);
    let mut buf = Vec::new();
    let mut current_date: Option<String> = None;
    let mut just_saw_header = false;

    while !remaining.is_empty() {
        buf.clear();
        let read = reader
            .read_until(0, &mut buf)
            .map_err(|error| format!("read git log history for {revision}: {error}"))?;
        if read == 0 {
            break;
        }
        if buf.last() == Some(&0) {
            buf.pop();
        }
        if buf.is_empty() {
            continue;
        }
        let header_slice = buf.strip_prefix(b"\n").unwrap_or(&buf);
        if let Some(after_rs) = header_slice.strip_prefix(b"\x1e") {
            let text = std::str::from_utf8(after_rs).unwrap_or_default().trim();
            current_date = text
                .get(..10)
                .filter(|date| is_iso_date(date))
                .map(ToOwned::to_owned);
            just_saw_header = true;
            continue;
        }
        let path_bytes = if just_saw_header {
            just_saw_header = false;
            buf.strip_prefix(b"\n").unwrap_or(&buf)
        } else {
            &buf
        };
        if path_bytes.is_empty() {
            continue;
        }
        let Some(date) = current_date.as_ref() else {
            continue;
        };
        if let Ok(path) = std::str::from_utf8(path_bytes)
            && remaining.remove(path)
        {
            dates.insert(path.to_string(), date.clone());
        }
    }

    // Fallback for any path touched only in a merge commit skipped by default
    // `git log --name-only`.
    for path in remaining {
        let output = run_git(
            workspace_root,
            &["log", "-1", "--format=%cI", revision, "--", &path],
        )
        .map_err(|error| format!("git log -1 for {path}: {error}"))?;
        if !output.success {
            return Err(format!(
                "git log -1 failed for {path}: {}",
                output.stderr.trim()
            ));
        }
        let raw = output.stdout.trim();
        if let Some(date) = raw.get(..10).filter(|date| is_iso_date(date)) {
            dates.insert(path, date.to_string());
        }
    }
    Ok(dates)
}

pub(super) fn read_single_blob(workspace_root: &Path, oid: &str) -> Result<Vec<u8>, String> {
    let mut batches = read_blob_batch(workspace_root, &[oid])?;
    batches
        .pop()
        .ok_or_else(|| format!("missing blob {oid} from git cat-file --batch"))
}

pub(super) fn read_blob_batch(
    workspace_root: &Path,
    oids: &[&str],
) -> Result<Vec<Vec<u8>>, String> {
    if oids.is_empty() {
        return Ok(Vec::new());
    }
    let mut stdin = Vec::with_capacity(oids.len() * 42);
    for oid in oids {
        stdin.extend_from_slice(oid.as_bytes());
        stdin.push(b'\n');
    }
    let mut file = run_git_captured(
        workspace_root,
        &["cat-file", "--batch"],
        Some(&stdin),
        GIT_LISTING_LIMIT,
    )?;
    let mut reader = BufReader::new(&mut file);
    let mut results = Vec::with_capacity(oids.len());
    for expected_oid in oids {
        let mut header = String::new();
        let read = reader
            .read_line(&mut header)
            .map_err(|error| format!("read cat-file header for {expected_oid}: {error}"))?;
        if read == 0 {
            return Err(format!(
                "unexpected EOF from git cat-file --batch for {expected_oid}"
            ));
        }
        let trimmed = header.trim_end_matches(['\r', '\n']);
        let parts: Vec<&str> = trimmed.split_ascii_whitespace().collect();
        if parts.len() < 3 || parts[1] != "blob" {
            return Err(format!(
                "git cat-file --batch returned non-blob header {trimmed:?} for {expected_oid}"
            ));
        }
        let size: usize = parts[2]
            .parse()
            .map_err(|error| format!("invalid blob size in {trimmed:?}: {error}"))?;
        let mut content = vec![0u8; size];
        reader
            .read_exact(&mut content)
            .map_err(|error| format!("read blob bytes for {expected_oid}: {error}"))?;
        // Consume the trailing LF emitted by `git cat-file --batch`.
        let mut lf = [0u8; 1];
        reader
            .read_exact(&mut lf)
            .map_err(|error| format!("read cat-file delimiter for {expected_oid}: {error}"))?;
        results.push(content);
    }
    Ok(results)
}

fn run_git_captured(
    workspace_root: &Path,
    args: &[&str],
    stdin_bytes: Option<&[u8]>,
    limit: u64,
) -> Result<std::fs::File, String> {
    let mut command =
        git_command(workspace_root, args).map_err(|error| format!("admit git command: {error}"))?;
    let stdin = match stdin_bytes {
        None => Stdio::null(),
        Some(bytes) => {
            let mut input = tempfile::tempfile()
                .map_err(|error| format!("create git stdin temp file: {error}"))?;
            input
                .write_all(bytes)
                .and_then(|()| input.seek(SeekFrom::Start(0)).map(drop))
                .map_err(|error| format!("write git stdin temp file: {error}"))?;
            Stdio::from(input)
        }
    };
    let mut stdout_file =
        tempfile::tempfile().map_err(|error| format!("create git stdout temp file: {error}"))?;
    let stdout_handle = stdout_file
        .try_clone()
        .map_err(|error| format!("clone git stdout temp file: {error}"))?;
    let mut stderr_file =
        tempfile::tempfile().map_err(|error| format!("create git stderr temp file: {error}"))?;
    let stderr_handle = stderr_file
        .try_clone()
        .map_err(|error| format!("clone git stderr temp file: {error}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let child = command
        .stdin(stdin)
        .stdout(stdout_handle)
        .stderr(stderr_handle)
        .spawn()
        .map_err(|error| format!("spawn `git {}`: {error}", args.join(" ")))?;

    let mut over_limit = || {
        let exceeded = stdout_file
            .metadata()
            .map(|meta| meta.len() > limit)
            .unwrap_or(true);
        exceeded.then_some(ProcessStopCause::Periodic)
    };
    let outcome = SupervisedChild::adopt(child)
        .supervise(Supervision {
            stdin: Stdin::Null,
            stdout: Stream::Redirected,
            stderr: Stream::Redirected,
            timeout: Some(GIT_SCAN_TIMEOUT),
            clock: None,
            every: Some(Periodic {
                interval: GIT_LIMIT_POLL,
                run: &mut over_limit,
            }),
            termination: Termination::KillNow,
            cancelled: None,
            intercept_signals: false,
            drain_budget: Duration::ZERO,
            cap_drain_at_deadline: false,
            require_eof: false,
            stopped_descendants: None,
            on_spawn: None,
            on_stopped_descendant: None,
            extendable_deadline: None,
        })
        .map_err(|error| format!("supervise `git {}`: {error}", args.join(" ")))?;

    if outcome.timed_out {
        return Err(format!("`git {}` timed out", args.join(" ")));
    }
    if outcome
        .stop_causes
        .iter()
        .any(|cause| matches!(cause, ProcessStopCause::Periodic))
    {
        return Err(format!(
            "`git {}` exceeded output cap of {limit} bytes",
            args.join(" ")
        ));
    }
    if !outcome.success {
        let mut stderr = String::new();
        let _ = stderr_file.seek(SeekFrom::Start(0));
        let _ = stderr_file.take(4096).read_to_string(&mut stderr);
        let detail = stderr.trim();
        return Err(if detail.is_empty() {
            format!(
                "`git {}` failed with exit code {:?}",
                args.join(" "),
                outcome.exit_code
            )
        } else {
            format!("`git {}` failed: {detail}", args.join(" "))
        });
    }
    let len = stdout_file
        .metadata()
        .map(|meta| meta.len())
        .unwrap_or(u64::MAX);
    if len > limit {
        return Err(format!(
            "`git {}` exceeded output cap of {limit} bytes",
            args.join(" ")
        ));
    }
    stdout_file
        .seek(SeekFrom::Start(0))
        .map_err(|error| format!("rewind git stdout temp file: {error}"))?;
    Ok(stdout_file)
}
