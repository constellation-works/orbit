//! Age-based pruning of the checkout's `.orbit/tmp`, run by worktree GC.
//!
//! `orbit gc tmp` empties the directory on request and refuses while any run
//! is active. This sweep runs unattended on every host, so it removes only
//! top-level entries that nothing has touched for the retention window and
//! leaves anything it cannot prove unused.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use orbit_common::fs::path::orbit_scratch_dir;
use orbit_engine::ScratchGcReport;
use orbit_store::contracts::JobRunQuery;

use crate::{OrbitError, OrbitRuntime};

impl OrbitRuntime {
    /// Prune top-level `.orbit/tmp` entries whose newest mtime is more than
    /// `retention_hours` old. A missing directory is an empty sweep.
    pub(crate) fn gc_scratch_by_age(
        &self,
        retention_hours: u64,
    ) -> Result<ScratchGcReport, OrbitError> {
        let too_large = || OrbitError::InvalidInput("scratch_older_than_hours is too large".into());
        let retention =
            Duration::from_secs(retention_hours.checked_mul(3600).ok_or_else(too_large)?);
        let cutoff = SystemTime::now()
            .checked_sub(retention)
            .ok_or_else(too_large)?;
        self.gc_scratch_before(cutoff, retention_hours)
    }

    fn gc_scratch_before(
        &self,
        cutoff: SystemTime,
        retention_hours: u64,
    ) -> Result<ScratchGcReport, OrbitError> {
        let checkout = self.paths().repo_root.canonicalize()?;
        let mut report = ScratchGcReport {
            path: orbit_scratch_dir(&checkout),
            retention_hours,
            bytes_reclaimed: 0,
            entries_removed: 0,
            entries_skipped: 0,
            entries_kept: 0,
            entries: Vec::new(),
        };
        // Read before any traversal: a run list that cannot be read means no
        // entry can be shown unnamed, so nothing is removed.
        let active_runs = self.active_run_documents()?;
        let since_epoch = cutoff.duration_since(UNIX_EPOCH).unwrap_or_default();
        let cutoff = (
            i64::try_from(since_epoch.as_secs()).unwrap_or(i64::MAX),
            i64::from(since_epoch.subsec_nanos()),
        );
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            filesystem::sweep(&checkout, cutoff, &active_runs, &mut report)?;
            Ok(report)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (cutoff, active_runs, &mut report);
            Err(OrbitError::InvalidInput(
                "scratch collection requires Linux or macOS directory-relative filesystem \
                 operations"
                    .into(),
            ))
        }
    }

    /// Every pending or running run's record and pipeline state, as JSON, so
    /// a path an active run was handed is found wherever it appears. Observed
    /// without reconciling: a stale owner still counts as active.
    fn active_run_documents(&self) -> Result<Vec<String>, OrbitError> {
        let runs = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            active_only: true,
            include_steps: false,
            ..JobRunQuery::default()
        })?;
        let mut documents = Vec::with_capacity(runs.len() * 2);
        for run in runs {
            documents.push(serde_json::to_string(&run).map_err(encode_error)?);
            if let Some(state) = self.read_run_state(&run.run_id)? {
                documents.push(serde_json::to_string(&state).map_err(encode_error)?);
            }
        }
        Ok(documents)
    }
}

fn encode_error(error: serde_json::Error) -> OrbitError {
    OrbitError::Execution(format!(
        "failed to encode an active run for scratch GC: {error}"
    ))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod filesystem {
    use std::collections::BTreeSet;
    use std::ffi::{CString, OsStr};
    use std::io;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};

    use orbit_engine::{ScratchGcEntry, ScratchGcReport};

    use super::super::tmp::filesystem::{names, open_directory, walk};
    use crate::OrbitError;

    /// An entry old enough to remove, pending the in-use checks.
    struct Stale {
        name: CString,
        path: PathBuf,
    }

    pub(super) fn sweep(
        checkout: &Path,
        cutoff: (i64, i64),
        active_runs: &[String],
        report: &mut ScratchGcReport,
    ) -> Result<(), OrbitError> {
        // Pin each directory separately so no symlink can redirect the walk
        // out of `.orbit/tmp`, as `orbit gc tmp` does.
        let root_name = CString::new(checkout.as_os_str().as_bytes())
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
        let root = open_directory(libc::AT_FDCWD, &root_name)?;
        let orbit = match open_directory(root.as_raw_fd(), c".orbit") {
            Ok(orbit) => orbit,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let tmp = match open_directory(orbit.as_raw_fd(), c"tmp") {
            Ok(tmp) => tmp,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let mut names = names(&tmp)?;
        names.sort();

        let mut stale = Vec::new();
        for name in names {
            let path = report.path.join(OsStr::from_bytes(name.to_bytes()));
            match walk(&tmp, &name, false) {
                Ok(tally) if tally.newest >= cutoff => report.entries_kept += 1,
                Ok(_) => stale.push(Stale { name, path }),
                Err(error) => skip(report, &path, format!("could not be measured: {error}")),
            }
        }
        if stale.is_empty() {
            return Ok(());
        }

        // Probed once, after measuring and right before removal, so the
        // window in which a process can open an entry stays small.
        let prefixes: Vec<&Path> = stale.iter().map(|entry| entry.path.as_path()).collect();
        let holders = find_holders(&prefixes);
        for (index, entry) in stale.iter().enumerate() {
            let reason = match &holders {
                Err(why) => Some(why.clone()),
                Ok(held) if held.contains(&index) => {
                    Some("held open by a live process, or the working directory of one".to_string())
                }
                Ok(_) if named_by_active_run(&entry.path, &entry.name, active_runs) => {
                    Some("named by an active run's state".to_string())
                }
                Ok(_) => None,
            };
            if let Some(reason) = reason {
                skip(report, &entry.path, reason);
                continue;
            }
            match walk(&tmp, &entry.name, true) {
                Ok(tally) => {
                    report.entries_removed += 1;
                    report.bytes_reclaimed = report.bytes_reclaimed.saturating_add(tally.bytes);
                    report.entries.push(ScratchGcEntry {
                        path: entry.path.to_string_lossy().into_owned(),
                        action: "removed".to_string(),
                        bytes_reclaimed: tally.bytes,
                        reason: None,
                    });
                }
                Err(error) => skip(
                    report,
                    &entry.path,
                    format!("removal failed, part of it may remain: {error}"),
                ),
            }
        }
        Ok(())
    }

    fn skip(report: &mut ScratchGcReport, path: &Path, reason: String) {
        report.entries_skipped += 1;
        report.entries.push(ScratchGcEntry {
            path: path.to_string_lossy().into_owned(),
            action: "skipped".to_string(),
            bytes_reclaimed: 0,
            reason: Some(reason),
        });
    }

    /// Whether any active run's record or state spells out this entry's
    /// path, its `.orbit/tmp`-relative path, or its bare name. A substring
    /// match, so it errs toward keeping the entry.
    fn named_by_active_run(path: &Path, name: &CString, active_runs: &[String]) -> bool {
        let name = name.to_string_lossy();
        let needles = [
            path.to_string_lossy().into_owned(),
            format!(".orbit/tmp/{name}"),
            format!("\"{name}\""),
        ];
        active_runs.iter().any(|document| {
            needles
                .iter()
                .any(|needle| document.contains(needle.as_str()))
        })
    }

    /// Indexes of `prefixes` that some process holds open, uses as its working
    /// directory, or runs as its executable. Best effort: a process the
    /// caller may not inspect (another user's) or that exits mid-scan is
    /// passed over. `Err` means the question could not be asked at all, which
    /// leaves every entry unproven.
    #[cfg(target_os = "linux")]
    fn find_holders(prefixes: &[&Path]) -> Result<BTreeSet<usize>, String> {
        let processes = std::fs::read_dir("/proc")
            .map_err(|error| format!("running processes could not be inspected: {error}"))?;
        let mut held = BTreeSet::new();
        let mut note = |target: &Path| {
            for (index, prefix) in prefixes.iter().enumerate() {
                if target.starts_with(prefix) {
                    held.insert(index);
                }
            }
        };
        for process in processes.flatten() {
            let is_pid = process
                .file_name()
                .as_bytes()
                .iter()
                .all(u8::is_ascii_digit);
            if !is_pid {
                continue;
            }
            let dir = process.path();
            for link in ["cwd", "exe"] {
                if let Ok(target) = std::fs::read_link(dir.join(link)) {
                    note(&target);
                }
            }
            if let Ok(descriptors) = std::fs::read_dir(dir.join("fd")) {
                for descriptor in descriptors.flatten() {
                    if let Ok(target) = std::fs::read_link(descriptor.path()) {
                        note(&target);
                    }
                }
            }
        }
        Ok(held)
    }

    /// A whole-machine `lsof` is slow but finite. Past this the probe fails
    /// closed and every stale entry is kept.
    #[cfg(target_os = "macos")]
    const LSOF_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
    /// Retained `lsof -F n` output, one line per open file on the host.
    #[cfg(target_os = "macos")]
    const LSOF_OUTPUT_LIMIT: usize = 128 * 1024 * 1024;

    #[cfg(target_os = "macos")]
    fn find_holders(prefixes: &[&Path]) -> Result<BTreeSet<usize>, String> {
        use orbit_common::process::output_capture::OUTPUT_TRUNCATED_MARKER;
        use orbit_common::process::run_bounded_capped;

        let mut command = std::process::Command::new("lsof");
        command.args(["-nP", "-w", "-F", "n"]);
        let output = run_bounded_capped(&mut command, LSOF_TIMEOUT, LSOF_OUTPUT_LIMIT)
            .map_err(|error| format!("open files could not be listed with lsof: {error}"))?;
        if !output.status.success() && output.stdout.is_empty() {
            return Err("open files could not be listed: lsof produced no output".to_string());
        }
        // A listing cut short could omit the very file that holds an entry.
        if output.stdout.ends_with(OUTPUT_TRUNCATED_MARKER) {
            return Err(
                "open files could not be listed: lsof output exceeded the capture limit"
                    .to_string(),
            );
        }
        let mut held = BTreeSet::new();
        for line in output.stdout.split(|byte| *byte == b'\n') {
            let Some(target) = line.strip_prefix(b"n") else {
                continue;
            };
            let target = Path::new(OsStr::from_bytes(target));
            for (index, prefix) in prefixes.iter().enumerate() {
                if target.starts_with(prefix) {
                    held.insert(index);
                }
            }
        }
        Ok(held)
    }
}
