//! Bounded source facts from Git and provider-owned PR identities.

use orbit_automation::{AutomationError, delivery::digest};
use orbit_common::fs::git::{
    GIT_FETCH_CAS_ATTEMPTS, git_fetch_cas_retry_delay, should_retry_git_ref_cas,
    with_git_fetch_lock,
};
use orbit_types::workflow::automation::recovery::{HistoryMapping, HistoryReplayRecord, refusal};
use orbit_types::workflow::automation::*;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::{BufRead, Read, Seek, SeekFrom, Write},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// Overall budget for every git and provider command one source pass runs.
const SOURCE_DEADLINE: Duration = Duration::from_secs(30);
/// Budget for one local git or provider command. A fetch uses the time left
/// on [`SOURCE_DEADLINE`] instead: two seconds is not a network fetch.
const COMMAND_BUDGET: Duration = Duration::from_secs(2);

/// Largest stdout one command's evidence may carry.
const OUTPUT_LIMIT: u64 = 1_048_576;
/// Largest listing [`Source::git_matching_paths`] streams through a filter.
/// Only the kept paths are held in memory, so this bounds disk, not evidence.
const LISTING_LIMIT: u64 = 256 * 1_048_576;

/// A delivery pass could not fetch `origin/<branch>`. Observation is not
/// advanced and the local branch is not consulted in its place.
pub(crate) const SOURCE_FETCH_FAILED: &str = "source_fetch_failed";

pub(crate) struct Source<'a> {
    root: &'a Path,
    started: Instant,
}

impl<'a> Source<'a> {
    pub(crate) fn new(root: &'a Path) -> Self {
        Self {
            root,
            started: Instant::now(),
        }
    }

    /// Run one bounded child process, capturing stdout under a size and time budget.
    ///
    /// A failing command defers with its command line and the first line of
    /// its stderr, so an operator reading a sweep row or `auto-task show`
    /// learns which ref git could not resolve instead of a bare token.
    fn command(&self, program: &str, args: &[&str]) -> Result<String, AutomationError> {
        self.command_with(program, args, &[], None, COMMAND_BUDGET)
    }

    fn command_with(
        &self,
        program: &str,
        args: &[&str],
        env: &[(&str, &str)],
        input: Option<&[u8]>,
        budget: Duration,
    ) -> Result<String, AutomationError> {
        self.command_with_output(program, args, env, input, budget, true)
    }

    /// `command_with`, optionally preserving stdout exactly. Trimming is right
    /// for a single revision or ref, and wrong for a NUL-delimited path list
    /// or instruction bytes whose leading or trailing whitespace is content.
    fn command_with_output(
        &self,
        program: &str,
        args: &[&str],
        env: &[(&str, &str)],
        input: Option<&[u8]>,
        budget: Duration,
        trim_output: bool,
    ) -> Result<String, AutomationError> {
        let mut file = self.capture(program, args, env, input, budget, OUTPUT_LIMIT)?;

        file.seek(SeekFrom::Start(0))
            .map_err(|e| AutomationError::Deferred(e.to_string()))?;

        let mut result = String::new();
        file.take(OUTPUT_LIMIT + 1)
            .read_to_string(&mut result)
            .map_err(|e| AutomationError::Deferred(e.to_string()))?;

        if result.len() as u64 > OUTPUT_LIMIT {
            return Err(AutomationError::Deferred("source_budget".into()));
        }

        Ok(if trim_output {
            result.trim().into()
        } else {
            result
        })
    }

    /// Run one child process to completion with stdout in an unlinked temp
    /// file, killing it once the file passes `limit` bytes or `budget` /
    /// [`SOURCE_DEADLINE`] elapses.
    fn capture(
        &self,
        program: &str,
        args: &[&str],
        env: &[(&str, &str)],
        input: Option<&[u8]>,
        budget: Duration,
        limit: u64,
    ) -> Result<std::fs::File, AutomationError> {
        // Test seam for ORB-14356: the next `diff-tree` fails as a deadline
        // once, so a swallowed canonical signature is distinguishable from a
        // propagated one. Production builds do not include the seam.
        #[cfg(test)]
        if canonical_signature_deadline_armed(args) {
            return Err(AutomationError::Deferred("source_deadline".into()));
        }

        #[cfg(test)]
        if source_deadline_forced(self) {
            return Err(AutomationError::Deferred("source_deadline".into()));
        }

        if self.started.elapsed() > SOURCE_DEADLINE {
            return Err(AutomationError::Deferred("source_deadline".into()));
        }

        let stdin = match input {
            None => Stdio::null(),
            Some(bytes) => {
                let mut stdin =
                    tempfile::tempfile().map_err(|e| AutomationError::Deferred(e.to_string()))?;
                stdin
                    .write_all(bytes)
                    .and_then(|()| stdin.seek(SeekFrom::Start(0)).map(drop))
                    .map_err(|e| AutomationError::Deferred(e.to_string()))?;
                Stdio::from(stdin)
            }
        };

        let file = tempfile::tempfile().map_err(|e| AutomationError::Deferred(e.to_string()))?;
        let output = file
            .try_clone()
            .map_err(|e| AutomationError::Deferred(e.to_string()))?;
        let mut diagnostics =
            tempfile::tempfile().map_err(|e| AutomationError::Deferred(e.to_string()))?;
        let errors = diagnostics
            .try_clone()
            .map_err(|e| AutomationError::Deferred(e.to_string()))?;
        // Keep each value as a separate OS argument. This avoids treating the
        // collected values as a command-line string while retaining Git's
        // normal argument semantics.
        let mut command = Command::new(program);
        for arg in args {
            command.arg(arg);
        }
        for (key, value) in env {
            command.env(key, value);
        }

        let mut child = command
            .current_dir(self.root)
            .stdin(stdin)
            .stdout(output)
            .stderr(errors)
            .spawn()
            .map_err(|e| {
                AutomationError::Deferred(format!(
                    "source_spawn_failed: {}: {e}",
                    command_line(program, args)
                ))
            })?;

        let start = Instant::now();

        loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|e| AutomationError::Deferred(e.to_string()))?
            {
                if !status.success() {
                    return Err(AutomationError::Deferred(format!(
                        "evidence_unavailable: {}: {}",
                        command_line(program, args),
                        failure_text(&mut diagnostics, status)
                    )));
                }
                break;
            }

            let over_budget = start.elapsed() > budget
                || self.started.elapsed() > SOURCE_DEADLINE
                || file
                    .metadata()
                    .map(|meta| meta.len() > limit)
                    .unwrap_or(true);

            if over_budget {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AutomationError::Deferred("source_budget".into()));
            }

            std::thread::sleep(Duration::from_millis(10));
        }

        // A child that exits between the last poll and here may have written
        // past the limit.
        if file
            .metadata()
            .map(|meta| meta.len() > limit)
            .unwrap_or(true)
        {
            return Err(AutomationError::Deferred("source_budget".into()));
        }

        Ok(file)
    }

    /// The NUL-delimited paths of a git listing for which `keep` is true, in
    /// listing order. The listing is streamed from disk and filtered, so only
    /// the matches are held in memory and the evidence cap on one command's
    /// output does not apply to the whole listing — a large tree lists far
    /// more paths than any caller keeps. The listing runs on the time left on
    /// this source's deadline, like a replay.
    pub(crate) fn git_matching_paths(
        &self,
        args: &[&str],
        keep: impl Fn(&str) -> bool,
    ) -> Result<Vec<String>, AutomationError> {
        let budget = match SOURCE_DEADLINE.checked_sub(self.started.elapsed()) {
            Some(budget) if !budget.is_zero() => budget,
            _ => return Err(AutomationError::Deferred("source_deadline".into())),
        };
        let mut file = self.capture("git", args, &[], None, budget, LISTING_LIMIT)?;
        file.seek(SeekFrom::Start(0))
            .map_err(|e| AutomationError::Deferred(e.to_string()))?;

        let mut paths = Vec::new();
        let mut record = Vec::new();
        let mut reader = std::io::BufReader::new(file);
        loop {
            record.clear();
            let read = reader
                .read_until(0, &mut record)
                .map_err(|e| AutomationError::Deferred(e.to_string()))?;
            if read == 0 {
                break;
            }
            if record.last() == Some(&0) {
                record.pop();
            }
            let path = std::str::from_utf8(&record)
                .map_err(|e| AutomationError::Evidence(e.to_string()))?;
            if !path.is_empty() && keep(path) {
                paths.push(path.to_string());
            }
        }
        Ok(paths)
    }

    pub(crate) fn git(&self, args: &[&str]) -> Result<String, AutomationError> {
        self.command("git", args)
    }

    /// [`Self::git`] without trimming stdout. Use when leading or trailing
    /// bytes are data: a NUL-delimited path list, or instruction file content.
    pub(crate) fn git_preserving_output(&self, args: &[&str]) -> Result<String, AutomationError> {
        self.command_with_output("git", args, &[], None, COMMAND_BUDGET, false)
    }

    /// [`Self::git`] with `input` on standard input, under the same budget.
    pub(crate) fn git_with_input(
        &self,
        args: &[&str],
        input: &[u8],
    ) -> Result<String, AutomationError> {
        self.command_with("git", args, &[], Some(input), COMMAND_BUDGET)
    }

    /// One git command for the replay proof. A batched range can outlive the
    /// two-second local-command budget, so this uses the time left on this
    /// source's deadline — the same rule as a fetch.
    fn git_replay(&self, args: &[&str], input: Option<&[u8]>) -> Result<String, AutomationError> {
        let budget = match SOURCE_DEADLINE.checked_sub(self.started.elapsed()) {
            Some(budget) if !budget.is_zero() => budget,
            _ => return Err(AutomationError::Deferred("source_deadline".into())),
        };
        self.command_with("git", args, &[], input, budget)
    }

    pub(crate) fn revision(&self, spec: &str) -> Result<SourceRevision, AutomationError> {
        let commit = self.git(&[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{spec}^{{commit}}"),
        ])?;

        let tree = self.git(&[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{commit}^{{tree}}"),
        ])?;

        Ok(SourceRevision { commit, tree })
    }

    /// Resolve the configured integration branch to a commit, never the
    /// executor worktree HEAD. The failure names the ref and git's own text:
    /// a branch that does not exist is a definition error an operator has to
    /// fix, and the reason has to say so wherever it is surfaced.
    ///
    /// This reads `refs/heads` only. Delivery observation uses [`Self::head`],
    /// which follows `origin` when that remote exists.
    pub(crate) fn verify_branch(&self, branch: &str) -> Result<SourceRevision, AutomationError> {
        self.git(&["check-ref-format", "--branch", branch])?;
        self.revision(&format!("refs/heads/{branch}"))
    }

    /// The local branch head. Member preparation and a direct-landing intent
    /// are about the checkout's branch, so they must not fetch.
    pub(crate) fn local_head(
        &self,
        branch: &str,
    ) -> Result<(String, SourceRevision), AutomationError> {
        let head = self.verify_branch(branch)?;
        let repository = self.repository()?;
        Ok((repository, head))
    }

    /// The head a delivery consumer observes.
    ///
    /// When `origin` is configured, the pass fetches that one branch into
    /// `refs/remotes/origin/<branch>` and resolves the fetched object. The
    /// worktree, index and local branch stay untouched. A failed fetch defers
    /// as [`SOURCE_FETCH_FAILED`] and does not fall back to the local ref.
    /// With no remote, the head is `refs/heads/<branch>`.
    pub(crate) fn head(&self, branch: &str) -> Result<(String, SourceRevision), AutomationError> {
        self.git(&["check-ref-format", "--branch", branch])?;
        let head = if self.origin_url()?.is_some() {
            self.fetch_origin_branch(branch)?;
            self.revision(&format!("refs/remotes/origin/{branch}"))
                .map_err(fetch_failure_from)?
        } else {
            self.revision(&format!("refs/heads/{branch}"))?
        };
        let repository = self.repository()?;
        #[cfg(test)]
        expire_source_after_head(self);
        Ok((repository, head))
    }

    /// `remote.origin.url` when the repository has one.
    ///
    /// A missing key is "no remote". A deadline or any other source failure
    /// propagates, so a timed-out config read is not treated as a `git:`
    /// identity.
    fn origin_url(&self) -> Result<Option<String>, AutomationError> {
        match self.git(&["config", "--get", "remote.origin.url"]) {
            Ok(url) if !url.is_empty() => Ok(Some(url)),
            Ok(_) => Ok(None),
            Err(AutomationError::Deferred(reason)) if is_evidence_unavailable(&reason) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Fetch `origin/<branch>` into the remote-tracking ref and nothing else.
    ///
    /// The refspec is forced (`+`) so a non-fast-forward still lands and the
    /// ancestry check can report `history_diverged`. The fetch uses whatever
    /// remains of the source deadline, not the two-second local-command budget.
    fn fetch_origin_branch(&self, branch: &str) -> Result<(), AutomationError> {
        let budget = match SOURCE_DEADLINE.checked_sub(self.started.elapsed()) {
            Some(budget) if !budget.is_zero() => budget,
            _ => return Err(AutomationError::Deferred("source_deadline".into())),
        };
        let spec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
        let root = self.root.to_path_buf();
        match with_git_fetch_lock(&root, || self.fetch_origin_branch_locked(&spec, budget)) {
            Ok(()) => Ok(()),
            Err(FetchLockError::Io(error)) => Err(fetch_failure(&error.to_string())),
            Err(FetchLockError::Deferred(error)) => Err(error),
        }
    }

    fn fetch_origin_branch_locked(
        &self,
        spec: &str,
        budget: Duration,
    ) -> Result<(), FetchLockError> {
        let args = [
            "fetch",
            "--no-tags",
            "--no-recurse-submodules",
            "origin",
            spec,
        ];
        let mut last_error = AutomationError::Deferred(SOURCE_FETCH_FAILED.into());
        for attempt in 0..GIT_FETCH_CAS_ATTEMPTS {
            match self.command_with("git", &args, &[("GIT_TERMINAL_PROMPT", "0")], None, budget) {
                Ok(_) => return Ok(()),
                Err(error) => {
                    let text = match &error {
                        AutomationError::Deferred(reason) => reason.clone(),
                        other => other.to_string(),
                    };
                    let retry = should_retry_git_ref_cas(attempt, &text);
                    last_error = error;
                    if retry {
                        tracing::warn!(
                            attempt,
                            spec,
                            "retrying delivery source fetch after git ref update contention"
                        );
                        std::thread::sleep(git_fetch_cas_retry_delay());
                        continue;
                    }
                    break;
                }
            }
        }
        Err(FetchLockError::Deferred(fetch_failure_from(last_error)))
    }

    /// How far `observed` trails `refs/remotes/origin/<branch>`, without fetching.
    ///
    /// `None` when the repository has no origin. A missing remote-tracking ref
    /// is [`RemoteObservation::unfetched`], not an error, so doctor can name it.
    pub(crate) fn remote_observation(
        &self,
        branch: &str,
        observed: Option<&str>,
    ) -> Result<Option<RemoteObservation>, AutomationError> {
        if self.origin_url()?.is_none() {
            return Ok(None);
        }
        let remote = match self.revision(&format!("refs/remotes/origin/{branch}")) {
            Ok(revision) => revision.commit,
            Err(AutomationError::Deferred(reason)) if is_evidence_unavailable(&reason) => {
                return Ok(Some(RemoteObservation {
                    observed: observed.map(str::to_owned),
                    remote_head: None,
                    behind: None,
                    diverged: false,
                    unfetched: true,
                    oldest_unobserved_epoch: None,
                }));
            }
            Err(error) => return Err(error),
        };
        let Some(observed) = observed else {
            return Ok(Some(RemoteObservation {
                observed: None,
                remote_head: Some(remote),
                behind: None,
                diverged: false,
                unfetched: false,
                oldest_unobserved_epoch: None,
            }));
        };
        if observed == remote {
            return Ok(Some(RemoteObservation {
                observed: Some(observed.to_owned()),
                remote_head: Some(remote),
                behind: Some(0),
                diverged: false,
                unfetched: false,
                oldest_unobserved_epoch: None,
            }));
        }
        if !self.is_ancestor(observed, &remote)? {
            return Ok(Some(RemoteObservation {
                observed: Some(observed.to_owned()),
                remote_head: Some(remote),
                behind: None,
                diverged: true,
                unfetched: false,
                oldest_unobserved_epoch: None,
            }));
        }

        let range = format!("{observed}..{remote}");
        let behind = self
            .git(&["rev-list", "--first-parent", "--count", &range])?
            .parse::<u64>()
            .map_err(|_| AutomationError::Deferred("source_count_invalid".into()))?;
        let oldest_unobserved_epoch = if behind == 0 {
            None
        } else {
            // Git applies max-count before reverse, which would select the tip.
            // First-parent distance selects the oldest pending commit while
            // keeping command output constant-sized, even for a long trail.
            let oldest = format!("{remote}~{}", behind - 1);
            self.git(&["show", "-s", "--format=%ct", &oldest])?
                .parse::<i64>()
                .ok()
        };
        Ok(Some(RemoteObservation {
            observed: Some(observed.to_owned()),
            remote_head: Some(remote),
            behind: Some(behind),
            diverged: false,
            unfetched: false,
            oldest_unobserved_epoch,
        }))
    }

    /// `true` when `older` is an ancestor of `newer`. A missing object is
    /// divergence, not success. A source deadline propagates.
    pub(super) fn is_ancestor(&self, older: &str, newer: &str) -> Result<bool, AutomationError> {
        match self.git(&["merge-base", "--is-ancestor", older, newer]) {
            Ok(_) => Ok(true),
            Err(AutomationError::Deferred(reason)) if is_evidence_unavailable(&reason) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// The stable repository identity shared by delivery observation and
    /// review certificates: a GitHub remote gives the provider-visible name;
    /// anything else is identified by a digest of its remote or git dir.
    pub(crate) fn repository(&self) -> Result<String, AutomationError> {
        let remote = self.origin_url()?.unwrap_or_default();

        let normalized = remote.trim_end_matches(".git");
        let repository = if let Some(github) = normalized
            .strip_prefix("https://github.com/")
            .or_else(|| normalized.strip_prefix("git@github.com:"))
        {
            github.to_string()
        } else {
            let identity = if remote.is_empty() {
                self.git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?
            } else {
                remote
            };
            format!("git:{}", digest(identity.as_bytes()))
        };

        Ok(repository)
    }

    pub(super) fn observe(
        &self,
        branch: &str,
        state: &AutomationState,
    ) -> Result<SourcePage, AutomationError> {
        self.observe_with_lookup(branch, state, &|repository, sha| {
            let request = orbit_tools::github_cli::commit_pull_requests_request(repository, sha)?;
            self.command(
                "gh",
                &request.args.iter().map(String::as_str).collect::<Vec<_>>(),
            )
        })
    }

    pub(super) fn observe_with_lookup(
        &self,
        branch: &str,
        state: &AutomationState,
        lookup: &dyn Fn(&str, &str) -> Result<String, AutomationError>,
    ) -> Result<SourcePage, AutomationError> {
        self.observe_with_lookup_limit(branch, state, lookup, 10, None, false)
    }

    fn observe_with_lookup_limit(
        &self,
        branch: &str,
        state: &AutomationState,
        lookup: &dyn Fn(&str, &str) -> Result<String, AutomationError>,
        lookup_limit: usize,
        head_override: Option<SourceRevision>,
        preserve_known_associations: bool,
    ) -> Result<SourcePage, AutomationError> {
        let (repository, configured_head) = self.head(branch)?;
        let head = head_override.unwrap_or(configured_head);

        if repository != state.repository {
            return Err(AutomationError::Deferred("repository_changed".into()));
        }

        self.git(&[
            "merge-base",
            "--is-ancestor",
            &state.observed.commit,
            &head.commit,
        ])
        .map_err(|_| AutomationError::Deferred("history_diverged".into()))?;

        let range = format!("{}..{}", state.observed.commit, head.commit);

        // Count uses bounded output and the same command deadline. Select the oldest
        // page by first-parent distance; reverse+max-count alone selects the newest.
        let count = self
            .git(&["rev-list", "--first-parent", "--count", &range])?
            .parse::<usize>()
            .map_err(|_| AutomationError::Deferred("source_count_invalid".into()))?;

        let through = if count > 200 {
            self.revision(&format!("{}~{}", head.commit, count - 200))?
        } else {
            head
        };

        let page_range = format!("{}..{}", state.observed.commit, through.commit);
        let commits = self
            .git(&[
                "rev-list",
                "--first-parent",
                "--reverse",
                "--max-count=200",
                &page_range,
            ])?
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();

        let mut unresolved = BTreeMap::new();
        let mut associations = state.associations.clone();

        // Each pass resolves the newest commits plus a rotating slice of the
        // previously unresolved ones, so no commit is starved of retries.
        let old = state.unresolved.keys().collect::<Vec<_>>();
        let offset = (state.generation as usize) % old.len().max(1);
        let candidates = commits
            .iter()
            .take(lookup_limit)
            .chain(
                old.iter()
                    .cycle()
                    .skip(offset)
                    .take(old.len().min(10))
                    .copied(),
            )
            .filter(|sha| !preserve_known_associations || !associations.contains_key(*sha))
            .cloned()
            .collect::<Vec<_>>();

        for sha in &candidates {
            if self.started.elapsed() > Duration::from_secs(20) {
                break;
            }

            // Without a provider-visible repository there is no identity to ask for.
            if repository.starts_with("git:") {
                unresolved.insert(sha.clone(), "delivery_owner_evidence_pending".into());
                continue;
            }

            let raw = match lookup(&repository, sha) {
                Ok(raw) => raw,
                Err(_) => {
                    unresolved.insert(sha.clone(), "evidence_unavailable".into());
                    continue;
                }
            };

            let prs: Value =
                serde_json::from_str(&raw).map_err(|e| AutomationError::Deferred(e.to_string()))?;

            // Exactly one merged pull request into this branch is an identity;
            // several are ambiguous and none simply leaves the commit unresolved.
            let matching = prs
                .as_array()
                .ok_or_else(|| AutomationError::Deferred("provider_response_invalid".into()))?
                .iter()
                .filter(|pr| {
                    pr["merged_at"].is_string()
                        && pr["base"]["ref"].as_str() == Some(branch)
                        && pr["base"]["repo"]["full_name"].as_str() == Some(&repository)
                })
                .collect::<Vec<_>>();

            let association = match matching.as_slice() {
                [] => None,
                [pr] => Some(super::provider::association(pr, &repository, branch)?),
                _ => {
                    unresolved.insert(sha.clone(), "provider_identity_ambiguous".into());
                    continue;
                }
            };

            associations.insert(sha.clone(), association);
            unresolved.insert(sha.clone(), "landing_span_pending".into());
        }

        let deliveries =
            super::provider::group(self, state, &repository, branch, &commits, &associations)?;

        for sha in &commits {
            if !deliveries
                .iter()
                .any(|delivery| delivery.commits.contains(sha))
            {
                unresolved
                    .entry(sha.clone())
                    .or_insert_with(|| "evidence_pending".into());
            }
        }

        Ok(SourcePage {
            from: state.observed.clone(),
            through,
            commits,
            deliveries,
            associations,
            unresolved,
            exclusions: Default::default(),
            complete: count <= 200,
        })
    }

    /// Prove and collect a bounded canonical replay of a diverged consumer.
    /// Provider association uses the same path as ordinary observation, but
    /// every commit in the bounded repair range must be resolved in this pass.
    pub(super) fn replay_history_with_lookup(
        &self,
        branch: &str,
        state: &AutomationState,
        lookup: &dyn Fn(&Source<'_>, &str, &str) -> Result<String, AutomationError>,
        accepted_receipts: usize,
    ) -> Result<(SourcePage, HistoryReplayRecord), AutomationError> {
        let (repository, head) = self.head(branch)?;
        if repository != state.repository {
            return Err(AutomationError::Refused(refusal::REPOSITORY_CHANGED.into()));
        }

        // `head` may already have spent the shared deadline on a fetch. The
        // proof is a separate pass: a range the traversal check admits gets a
        // full deadline, and its git reads are batched under that budget.
        let source = Self::new(self.root);

        let old_observed = source
            .revision(&state.observed.commit)
            .map_err(|_| AutomationError::Refused(refusal::HISTORY_OBJECT_MISSING.into()))?;
        if old_observed != state.observed {
            return Err(AutomationError::Refused(
                refusal::HISTORY_CONTRACT_DRIFT.into(),
            ));
        }
        if source
            .git(&[
                "merge-base",
                "--is-ancestor",
                &old_observed.commit,
                &head.commit,
            ])
            .is_ok()
        {
            return Err(AutomationError::Refused(
                refusal::HISTORY_NOT_DIVERGED.into(),
            ));
        }

        for boundary in
            [&state.covered, &state.baseline]
                .into_iter()
                .chain(state.active.iter().flat_map(|active| {
                    [
                        &active.batch.from_exclusive,
                        &active.batch.through_inclusive,
                    ]
                }))
        {
            if !matches!(source.revision(&boundary.commit), Ok(actual) if actual == *boundary)
                || source
                    .git(&[
                        "merge-base",
                        "--is-ancestor",
                        &boundary.commit,
                        &head.commit,
                    ])
                    .is_err()
            {
                return Err(AutomationError::Refused(
                    refusal::HISTORY_BOUNDARY_UNREACHABLE.into(),
                ));
            }
        }

        let base_commit = source.git(&["merge-base", &old_observed.commit, &head.commit])?;
        if source
            .git(&[
                "merge-base",
                "--is-ancestor",
                &state.covered.commit,
                &base_commit,
            ])
            .is_err()
        {
            return Err(AutomationError::Refused(
                refusal::HISTORY_BOUNDARY_UNREACHABLE.into(),
            ));
        }
        let common_base = source.revision(&base_commit)?;
        let old_commits = source.first_parent_range(&base_commit, &old_observed.commit)?;
        let canonical_commits = source.first_parent_range(&base_commit, &head.commit)?;
        validate_replay_range_lengths(old_commits.len(), canonical_commits.len())?;

        let canonical_proofs = source.replay_signatures(&canonical_commits)?;
        let mut candidates_by_proof = BTreeMap::<String, Vec<(usize, String)>>::new();
        for (position, (canonical, proof)) in
            canonical_commits.iter().zip(canonical_proofs).enumerate()
        {
            match proof {
                Ok(proof) => {
                    candidates_by_proof
                        .entry(proof)
                        .or_default()
                        .push((position, canonical.clone()));
                }
                // A commit with no `.orbit` tree is not a mapping candidate.
                // A deadline or command budget is the pass failing, not a miss.
                Err(error) if !is_source_deadline_or_budget(&error) => continue,
                Err(error) => return Err(error),
            }
        }

        let orphan_proofs = source.replay_signatures(&old_commits)?;
        let mut pending = Vec::with_capacity(old_commits.len());
        let mut last_position = None;
        for (orphan, proof) in old_commits.iter().zip(orphan_proofs) {
            let proof_digest = proof?;
            let candidates = candidates_by_proof
                .get(&proof_digest)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let (position, canonical) = unique_mapping_candidate(candidates, last_position)?;
            last_position = Some(position);
            pending.push((orphan.clone(), canonical.to_owned(), proof_digest));
        }

        let mut revision_ids = Vec::with_capacity(pending.len() * 2);
        for (orphan, canonical, _) in &pending {
            revision_ids.push(orphan.clone());
            revision_ids.push(canonical.clone());
        }
        let mut revisions = source.replay_revisions(&revision_ids)?.into_iter();
        let mut mappings = Vec::with_capacity(pending.len());
        for (_orphan, _canonical, proof_digest) in pending {
            let (Some(orphan), Some(canonical)) = (revisions.next(), revisions.next()) else {
                return Err(AutomationError::Deferred(
                    "evidence_unavailable: git cat-file --batch-check: replay revision count"
                        .into(),
                ));
            };
            mappings.push(HistoryMapping {
                orphan,
                canonical,
                proof_digest,
            });
        }

        let canonical_by_orphan = mappings
            .iter()
            .map(|mapping| {
                (
                    mapping.orphan.commit.as_str(),
                    mapping.canonical.commit.as_str(),
                )
            })
            .collect::<BTreeMap<_, _>>();

        let mut probe = state.clone();
        probe.observed = common_base.clone();
        probe.pending_commits.clear();
        probe.pending.clear();
        probe.waived.clear();
        probe.excluded.clear();
        probe.unresolved.clear();
        probe.associations.clear();
        for mapping in &mappings {
            if let Some(Some(association)) = state.associations.get(&mapping.orphan.commit) {
                // A delivery can span several commits, all retaining the same
                // anchor. Resolve it from the complete proof, not this commit's
                // mapping, and refuse an anchor the proof cannot account for.
                let anchor = canonical_by_orphan
                    .get(association.anchor.as_str())
                    .ok_or_else(|| {
                        AutomationError::Refused(refusal::HISTORY_CONTRACT_DRIFT.into())
                    })?;
                let mut association = association.clone();
                association.anchor = (*anchor).to_owned();
                probe
                    .associations
                    .insert(mapping.canonical.commit.clone(), Some(association));
                continue;
            }
            if let Some(delivery) = state
                .pending
                .iter()
                .chain(&state.waived)
                .find(|delivery| delivery.after.commit == mapping.orphan.commit)
            {
                probe.associations.insert(
                    mapping.canonical.commit.clone(),
                    Some(DeliveryAssociation {
                        key: delivery.key.clone(),
                        anchor: mapping.canonical.commit.clone(),
                        reference: delivery.evidence_reference.clone(),
                        landed_at: delivery.landed_at,
                    }),
                );
                continue;
            }

            // A strongly mapped unresolved commit is already an identified
            // obligation even though no provider owns it. Mark its canonical
            // replacement as known so ordinary observation does not demand
            // fresh provider proof for the same debt. The exact reason is
            // restored on the page below; inserted commits still take the
            // normal provider lookup path.
            if state.unresolved.contains_key(&mapping.orphan.commit) {
                probe
                    .associations
                    .insert(mapping.canonical.commit.clone(), None);
            }
        }
        probe.active = None;
        let replay_through = mappings
            .last()
            .map(|mapping| mapping.canonical.clone())
            .ok_or_else(|| AutomationError::Refused(refusal::HISTORY_MAPPING_AMBIGUOUS.into()))?;
        // Provider observation can fetch `origin` again. Give that pass its
        // own deadline so it cannot consume the signature proof's budget.
        let page_source = Self::new(self.root);
        let mut page = page_source
            .observe_with_lookup_limit(
                branch,
                &probe,
                &|repository, sha| lookup(&page_source, repository, sha),
                200,
                Some(replay_through.clone()),
                true,
            )
            .map_err(|_| AutomationError::Refused(refusal::PROVIDER_PROOF_UNAVAILABLE.into()))?;

        for mapping in &mappings {
            let Some(reason) = state.unresolved.get(&mapping.orphan.commit) else {
                continue;
            };

            page.unresolved
                .insert(mapping.canonical.commit.clone(), reason.clone());
            if !state.associations.contains_key(&mapping.orphan.commit) {
                page.associations.remove(&mapping.canonical.commit);
            }
        }

        Ok((
            page,
            HistoryReplayRecord {
                captured_generation: state.generation,
                captured_head: head.clone(),
                common_base,
                old_observed,
                new_observed: replay_through,
                mappings,
                added_obligations: vec![],
                unchanged_baseline: state.baseline.clone(),
                unchanged_covered: state.covered.clone(),
                accepted_receipts,
            },
        ))
    }

    pub(super) fn replay_history(
        &self,
        branch: &str,
        state: &AutomationState,
        accepted_receipts: usize,
    ) -> Result<(SourcePage, HistoryReplayRecord), AutomationError> {
        self.replay_history_with_lookup(
            branch,
            state,
            &|source, repository, sha| {
                let request =
                    orbit_tools::github_cli::commit_pull_requests_request(repository, sha)?;
                source.command(
                    "gh",
                    &request.args.iter().map(String::as_str).collect::<Vec<_>>(),
                )
            },
            accepted_receipts,
        )
    }

    fn first_parent_range(
        &self,
        from: &str,
        through: &str,
    ) -> Result<Vec<String>, AutomationError> {
        let range = format!("{from}..{through}");
        Ok(self
            .git(&[
                "rev-list",
                "--first-parent",
                "--reverse",
                "--max-count=1001",
                &range,
            ])?
            .lines()
            .map(str::to_owned)
            .collect())
    }

    /// Parent-relative signature of each commit: `.orbit` tree id, NUL, patch.
    ///
    /// Each entry is the digest, or a per-commit `evidence_unavailable` when
    /// that commit has no signature. A deadline or command budget fails the
    /// whole call — callers must not skip those.
    fn replay_signatures(
        &self,
        commits: &[String],
    ) -> Result<Vec<Result<String, AutomationError>>, AutomationError> {
        if commits.is_empty() {
            return Ok(Vec::new());
        }

        let trees = self.orbit_trees(commits)?;
        let mut present = Vec::new();
        for (index, tree) in trees.iter().enumerate() {
            if tree.is_some() {
                present.push(commits[index].clone());
            }
        }
        let patches = self.parent_patches(&present)?;
        let mut patches = patches.into_iter();
        let mut signatures = Vec::with_capacity(commits.len());
        for (commit, tree) in commits.iter().zip(trees) {
            let Some(tree) = tree else {
                signatures.push(Err(missing_orbit_tree(commit)));
                continue;
            };
            match patches.next() {
                Some(Ok(patch)) => {
                    signatures.push(Ok(digest(format!("{tree}\0{patch}").as_bytes())));
                }
                Some(Err(error)) => signatures.push(Err(error)),
                None => {
                    return Err(AutomationError::Deferred(
                        "evidence_unavailable: git diff-tree --stdin: replay patch count".into(),
                    ));
                }
            }
        }
        Ok(signatures)
    }

    /// Object id of `commit:.orbit` for each commit. `None` means the path is
    /// absent. A failed batch is a deadline, a budget, or a git failure.
    fn orbit_trees(&self, commits: &[String]) -> Result<Vec<Option<String>>, AutomationError> {
        let mut input = String::new();
        for commit in commits {
            input.push_str(commit);
            input.push_str(":.orbit\n");
        }
        let output = self.git_replay(
            &["cat-file", "--batch-check=%(objectname)"],
            Some(input.as_bytes()),
        )?;
        let lines: Vec<&str> = output.lines().collect();
        if lines.len() != commits.len() {
            return Err(AutomationError::Deferred(
                "evidence_unavailable: git cat-file --batch-check: replay orbit tree count".into(),
            ));
        }

        let mut trees = Vec::with_capacity(commits.len());
        for (commit, line) in commits.iter().zip(lines) {
            if is_object_name(line) {
                trees.push(Some(line.to_owned()));
                continue;
            }
            if line == format!("{commit}:.orbit missing") {
                trees.push(None);
                continue;
            }
            return Err(AutomationError::Deferred(format!(
                "evidence_unavailable: git cat-file --batch-check: {line}"
            )));
        }
        Ok(trees)
    }

    /// Parent-relative patch of each commit, in order. An empty diff is an
    /// empty string, matching `diff-tree --no-commit-id` on that commit.
    ///
    /// A batch that exceeds the command output cap is split. A deadline fails
    /// the call. One commit's own failure stays in its entry so the caller
    /// can skip a canonical miss or surface an orphan miss.
    fn parent_patches(
        &self,
        commits: &[String],
    ) -> Result<Vec<Result<String, AutomationError>>, AutomationError> {
        if commits.is_empty() {
            return Ok(Vec::new());
        }

        let mut input = String::new();
        for commit in commits {
            input.push_str(commit);
            input.push('\n');
        }
        match self.git_replay(
            &[
                "diff-tree",
                "--stdin",
                "--binary",
                "--full-index",
                "--no-renames",
                "-r",
            ],
            Some(input.as_bytes()),
        ) {
            Ok(output) => Ok(split_diff_tree_patches(commits, &output)
                .into_iter()
                .map(Ok)
                .collect()),
            Err(error) if is_source_deadline(&error) => Err(error),
            Err(error) if is_source_budget(&error) && commits.len() == 1 => Err(error),
            Err(_error) if commits.len() > 1 => {
                let mid = commits.len() / 2;
                let mut patches = self.parent_patches(&commits[..mid])?;
                patches.extend(self.parent_patches(&commits[mid..])?);
                Ok(patches)
            }
            Err(error) => Ok(vec![Err(error)]),
        }
    }

    /// Commit and tree ids for each already-resolved commit, in order.
    fn replay_revisions(&self, commits: &[String]) -> Result<Vec<SourceRevision>, AutomationError> {
        if commits.is_empty() {
            return Ok(Vec::new());
        }

        let mut input = String::new();
        for commit in commits {
            input.push_str(commit);
            input.push_str("^{commit}\n");
            input.push_str(commit);
            input.push_str("^{tree}\n");
        }
        let output = self.git_replay(
            &["cat-file", "--batch-check=%(objectname) %(objecttype)"],
            Some(input.as_bytes()),
        )?;
        let lines: Vec<&str> = output.lines().collect();
        if lines.len() != commits.len() * 2 {
            return Err(AutomationError::Deferred(
                "evidence_unavailable: git cat-file --batch-check: replay revision count".into(),
            ));
        }

        let mut revisions = Vec::with_capacity(commits.len());
        for (index, commit) in commits.iter().enumerate() {
            let commit_line = lines[index * 2];
            let tree_line = lines[index * 2 + 1];
            let (commit_id, commit_kind) = object_and_type(commit_line).ok_or_else(|| {
                AutomationError::Deferred(format!(
                    "evidence_unavailable: git cat-file --batch-check: {commit} is not a commit"
                ))
            })?;
            let (tree_id, tree_kind) = object_and_type(tree_line).ok_or_else(|| {
                AutomationError::Deferred(format!(
                    "evidence_unavailable: git cat-file --batch-check: {commit} has no tree"
                ))
            })?;
            if commit_kind != "commit" || tree_kind != "tree" {
                return Err(AutomationError::Deferred(format!(
                    "evidence_unavailable: git cat-file --batch-check: {commit} resolved to {commit_kind}/{tree_kind}"
                )));
            }
            revisions.push(SourceRevision {
                commit: commit_id.to_owned(),
                tree: tree_id.to_owned(),
            });
        }
        Ok(revisions)
    }

    /// Pin the batch boundaries so the frozen input stays reachable during review.
    pub(super) fn retain_batch(&self, batch: &CoverageBatch) -> Result<(), AutomationError> {
        let prefix = format!(
            "refs/orbit/automation/{}/{}",
            digest(batch.consumer.as_bytes()),
            batch.id
        );

        self.git(&[
            "update-ref",
            &format!("{prefix}/from"),
            &batch.from_exclusive.commit,
        ])?;

        self.git(&[
            "update-ref",
            &format!("{prefix}/through"),
            &batch.through_inclusive.commit,
        ])?;

        Ok(())
    }

    /// Pinned refs this consumer holds for its frozen batches.
    pub(super) fn retained_refs(&self, consumer: &str) -> Result<Vec<String>, AutomationError> {
        let prefix = format!("refs/orbit/automation/{}/", digest(consumer.as_bytes()));

        Ok(self
            .git(&["for-each-ref", "--format=%(refname)", &prefix])?
            .lines()
            .map(str::to_owned)
            .collect())
    }

    /// Drop pins whose batch is being forgotten. They exist only to keep a
    /// frozen input reachable, so a pin that cannot be deleted is reported to
    /// the caller rather than failing the operation that forgot the batch.
    pub(super) fn release_refs(&self, refs: &[String]) -> Vec<String> {
        refs.iter()
            .filter(|name| self.git(&["update-ref", "-d", name]).is_err())
            .cloned()
            .collect()
    }

    pub(super) fn verify_batch(&self, batch: &CoverageBatch) -> Result<(), AutomationError> {
        if self.revision(&batch.from_exclusive.commit)? != batch.from_exclusive
            || self.revision(&batch.through_inclusive.commit)? != batch.through_inclusive
        {
            return Err(AutomationError::Deferred("source_revision_changed".into()));
        }

        let (_, head) = self.head(&batch.branch)?;

        // A non-ancestor is divergence. Deadline and budget stay deferred:
        // folding every merge-base failure into `history_diverged` would
        // settle a closed action during an outage.
        if !self.is_ancestor(&batch.through_inclusive.commit, &head.commit)? {
            return Err(AutomationError::Deferred("history_diverged".into()));
        }

        let range = format!(
            "{}..{}",
            batch.from_exclusive.commit, batch.through_inclusive.commit
        );

        let actual = self
            .git(&["rev-list", "--first-parent", "--reverse", &range])?
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();

        if actual != batch.commits {
            return Err(AutomationError::Deferred(
                "source_membership_changed".into(),
            ));
        }

        Ok(())
    }
}

/// Read-only comparison of a consumer's observed commit with the
/// remote-tracking head. Doctor uses this and does not fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteObservation {
    pub observed: Option<String>,
    pub remote_head: Option<String>,
    pub behind: Option<u64>,
    pub diverged: bool,
    pub unfetched: bool,
    /// Committer time, unix seconds, of the oldest first-parent commit the
    /// observed cursor has not reached. Present only when `behind` is positive.
    pub oldest_unobserved_epoch: Option<i64>,
}

/// [`with_git_fetch_lock`] requires `From<io::Error>`. Automation errors have
/// no such conversion, so the locked fetch carries both.
enum FetchLockError {
    Io(std::io::Error),
    Deferred(AutomationError),
}

impl From<std::io::Error> for FetchLockError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

fn is_evidence_unavailable(reason: &str) -> bool {
    reason.starts_with("evidence_unavailable:")
}

/// Frozen-batch mismatches. Operational failures, including a prefixed
/// `source_fetch_failed`, are not mismatches: evidence evaluation defers
/// them instead of rejecting coverage.
pub(super) fn is_batch_mismatch(error: &AutomationError) -> bool {
    matches!(
        deferred_reason(error),
        Some("source_revision_changed" | "history_diverged" | "source_membership_changed")
    )
}

fn deferred_reason(error: &AutomationError) -> Option<&str> {
    match error {
        AutomationError::Deferred(reason) => Some(reason.as_str()),
        _ => None,
    }
}

fn is_source_deadline(error: &AutomationError) -> bool {
    deferred_reason(error) == Some("source_deadline")
}

fn is_source_budget(error: &AutomationError) -> bool {
    deferred_reason(error) == Some("source_budget")
}

/// Deadline exhaustion, including the in-command check that reports
/// `source_budget` once the shared clock is already past the deadline.
fn is_source_deadline_or_budget(error: &AutomationError) -> bool {
    is_source_deadline(error) || is_source_budget(error)
}

fn missing_orbit_tree(commit: &str) -> AutomationError {
    AutomationError::Deferred(format!(
        "evidence_unavailable: git rev-parse --verify --end-of-options {commit}:.orbit: path '.orbit' does not exist"
    ))
}

fn is_object_name(line: &str) -> bool {
    let len = line.len();
    (len == 40 || len == 64) && line.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn object_and_type(line: &str) -> Option<(&str, &str)> {
    let (name, kind) = line.split_once(' ')?;
    if is_object_name(name) && !kind.is_empty() && !kind.contains(' ') {
        Some((name, kind))
    } else {
        None
    }
}

/// Patches from `git diff-tree --stdin`, one per commit, in request order.
///
/// `diff-tree` omits a commit whose patch is empty, so a missing header is an
/// empty patch — the same bytes as `--no-commit-id` on that commit. Headers
/// are recognized only as a full line equal to a still-pending commit id.
fn split_diff_tree_patches(commits: &[String], output: &str) -> Vec<String> {
    let lines: Vec<&str> = output.lines().collect();
    let mut index = 0;
    let mut patches = Vec::with_capacity(commits.len());
    for (nth, commit) in commits.iter().enumerate() {
        if index < lines.len() && lines[index] == commit.as_str() {
            index += 1;
            let start = index;
            while index < lines.len()
                && !commits[nth + 1..]
                    .iter()
                    .any(|later| lines[index] == later.as_str())
            {
                index += 1;
            }
            patches.push(lines[start..index].join("\n").trim().to_string());
        } else {
            patches.push(String::new());
        }
    }
    patches
}

#[cfg(test)]
thread_local! {
    static CANONICAL_SIGNATURE_DEADLINE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Fail the next `diff-tree` once with `source_deadline`.
///
/// The canonical pass runs that command first. Swallowing the failure lets
/// the orphan pass succeed and report an ambiguous mapping instead.
#[cfg(test)]
pub(super) fn arm_canonical_signature_deadline() {
    CANONICAL_SIGNATURE_DEADLINE.with(|armed| armed.set(true));
}

#[cfg(test)]
pub(super) fn clear_canonical_signature_deadline() {
    CANONICAL_SIGNATURE_DEADLINE.with(|armed| armed.set(false));
}

#[cfg(test)]
fn canonical_signature_deadline_armed(args: &[&str]) -> bool {
    if !args.contains(&"diff-tree") {
        return false;
    }
    CANONICAL_SIGNATURE_DEADLINE.with(|armed| armed.replace(false))
}

#[cfg(test)]
thread_local! {
    static EXPIRE_AFTER_NEXT_HEAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static EXPIRED_SOURCE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn arm_expire_source_after_next_head() {
    EXPIRE_AFTER_NEXT_HEAD.with(|armed| armed.set(true));
    EXPIRED_SOURCE.with(|source| source.set(0));
}

#[cfg(test)]
pub(super) fn clear_expired_source() {
    EXPIRE_AFTER_NEXT_HEAD.with(|armed| armed.set(false));
    EXPIRED_SOURCE.with(|source| source.set(0));
}

#[cfg(test)]
fn expire_source_after_head(source: &Source<'_>) {
    EXPIRE_AFTER_NEXT_HEAD.with(|armed| {
        if armed.replace(false) {
            EXPIRED_SOURCE.with(|expired| expired.set(source as *const Source<'_> as usize));
        }
    });
}

#[cfg(test)]
fn source_deadline_forced(source: &Source<'_>) -> bool {
    EXPIRED_SOURCE.with(|expired| expired.get() == source as *const Source<'_> as usize)
}

fn fetch_failure(detail: &str) -> AutomationError {
    let detail = detail.trim();
    if detail.is_empty() || detail == SOURCE_FETCH_FAILED {
        return AutomationError::Deferred(SOURCE_FETCH_FAILED.into());
    }
    if let Some(rest) = detail.strip_prefix("source_fetch_failed:") {
        let rest = rest.trim();
        if rest.is_empty() {
            return AutomationError::Deferred(SOURCE_FETCH_FAILED.into());
        }
        return AutomationError::Deferred(format!("{SOURCE_FETCH_FAILED}: {rest}"));
    }
    AutomationError::Deferred(format!("{SOURCE_FETCH_FAILED}: {detail}"))
}

fn fetch_failure_from(error: AutomationError) -> AutomationError {
    match error {
        AutomationError::Deferred(reason) => fetch_failure(&reason),
        other => fetch_failure(&other.to_string()),
    }
}

/// Bytes of a child's stderr kept for a deferral reason.
const FAILURE_TEXT_LIMIT: u64 = 4096;

fn command_line(program: &str, args: &[&str]) -> String {
    std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The first stderr line of a failed child, or its exit status when it said
/// nothing. Bounded so a chatty tool cannot turn a reason into a log.
fn failure_text(diagnostics: &mut std::fs::File, status: std::process::ExitStatus) -> String {
    let mut text = String::new();
    if diagnostics.seek(SeekFrom::Start(0)).is_ok() {
        let _ = diagnostics
            .take(FAILURE_TEXT_LIMIT)
            .read_to_string(&mut text);
    }

    match text.lines().map(str::trim).find(|line| !line.is_empty()) {
        Some(line) => line.to_string(),
        None => status.to_string(),
    }
}

pub(super) fn validate_replay_range_lengths(
    orphan_count: usize,
    canonical_count: usize,
) -> Result<(), AutomationError> {
    if orphan_count == 0 || orphan_count > 1000 || canonical_count > 1000 {
        return Err(AutomationError::Refused(
            refusal::HISTORY_TRAVERSAL_LIMIT.into(),
        ));
    }

    Ok(())
}

pub(super) fn unique_mapping_candidate(
    candidates: &[(usize, String)],
    last_position: Option<usize>,
) -> Result<(usize, &str), AutomationError> {
    let [(position, canonical)] = candidates else {
        return Err(AutomationError::Refused(
            refusal::HISTORY_MAPPING_AMBIGUOUS.into(),
        ));
    };
    if last_position.is_some_and(|last| *position <= last) {
        return Err(AutomationError::Refused(
            refusal::HISTORY_MAPPING_AMBIGUOUS.into(),
        ));
    }

    Ok((*position, canonical))
}
