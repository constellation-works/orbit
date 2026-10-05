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
    io::{Read, Seek, SeekFrom},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// Overall budget for every git and provider command one source pass runs.
const SOURCE_DEADLINE: Duration = Duration::from_secs(30);
/// Budget for one local git or provider command. A fetch uses the time left
/// on [`SOURCE_DEADLINE`] instead: two seconds is not a network fetch.
const COMMAND_BUDGET: Duration = Duration::from_secs(2);

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
        self.command_with(program, args, &[], COMMAND_BUDGET)
    }

    fn command_with(
        &self,
        program: &str,
        args: &[&str],
        env: &[(&str, &str)],
        budget: Duration,
    ) -> Result<String, AutomationError> {
        if self.started.elapsed() > SOURCE_DEADLINE {
            return Err(AutomationError::Deferred("source_deadline".into()));
        }

        let mut file =
            tempfile::tempfile().map_err(|e| AutomationError::Deferred(e.to_string()))?;
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
            .stdin(Stdio::null())
            .stdout(output)
            .stderr(errors)
            .spawn()
            .map_err(|e| {
                AutomationError::Deferred(format!(
                    "evidence_unavailable: {}: {e}",
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
                    .map(|meta| meta.len() > 1_048_576)
                    .unwrap_or(true);

            if over_budget {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AutomationError::Deferred("source_budget".into()));
            }

            std::thread::sleep(Duration::from_millis(10));
        }

        file.seek(SeekFrom::Start(0))
            .map_err(|e| AutomationError::Deferred(e.to_string()))?;

        let mut result = String::new();
        file.take(1_048_577)
            .read_to_string(&mut result)
            .map_err(|e| AutomationError::Deferred(e.to_string()))?;

        if result.len() > 1_048_576 {
            return Err(AutomationError::Deferred("source_budget".into()));
        }

        Ok(result.trim().into())
    }

    pub(crate) fn git(&self, args: &[&str]) -> Result<String, AutomationError> {
        self.command("git", args)
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
            match self.command_with("git", &args, &[("GIT_TERMINAL_PROMPT", "0")], budget) {
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
    fn is_ancestor(&self, older: &str, newer: &str) -> Result<bool, AutomationError> {
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
        lookup: &dyn Fn(&str, &str) -> Result<String, AutomationError>,
        accepted_receipts: usize,
    ) -> Result<(SourcePage, HistoryReplayRecord), AutomationError> {
        let (repository, head) = self.head(branch)?;
        if repository != state.repository {
            return Err(AutomationError::Refused(refusal::REPOSITORY_CHANGED.into()));
        }

        let old_observed = self
            .revision(&state.observed.commit)
            .map_err(|_| AutomationError::Refused(refusal::HISTORY_OBJECT_MISSING.into()))?;
        if old_observed != state.observed {
            return Err(AutomationError::Refused(
                refusal::HISTORY_CONTRACT_DRIFT.into(),
            ));
        }
        if self
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
            if !matches!(self.revision(&boundary.commit), Ok(actual) if actual == *boundary)
                || self
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

        let base_commit = self.git(&["merge-base", &old_observed.commit, &head.commit])?;
        if self
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
        let common_base = self.revision(&base_commit)?;
        let old_commits = self.first_parent_range(&base_commit, &old_observed.commit)?;
        let canonical_commits = self.first_parent_range(&base_commit, &head.commit)?;
        validate_replay_range_lengths(old_commits.len(), canonical_commits.len())?;

        let mut candidates_by_proof = BTreeMap::<String, Vec<(usize, String)>>::new();
        for (position, canonical) in canonical_commits.iter().enumerate() {
            if let Ok(proof) = self.replay_signature(canonical) {
                candidates_by_proof
                    .entry(proof)
                    .or_default()
                    .push((position, canonical.clone()));
            }
        }

        let mut mappings = Vec::with_capacity(old_commits.len());
        let mut last_position = None;
        for orphan in &old_commits {
            let proof_digest = self.replay_signature(orphan)?;
            let candidates = candidates_by_proof
                .get(&proof_digest)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let (position, canonical) = unique_mapping_candidate(candidates, last_position)?;
            last_position = Some(position);
            mappings.push(HistoryMapping {
                orphan: self.revision(orphan)?,
                canonical: self.revision(canonical)?,
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
        let mut page = self
            .observe_with_lookup_limit(
                branch,
                &probe,
                lookup,
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
            &|repository, sha| {
                let request =
                    orbit_tools::github_cli::commit_pull_requests_request(repository, sha)?;
                self.command(
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

    fn replay_signature(&self, commit: &str) -> Result<String, AutomationError> {
        let orbit_tree = self.git(&[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{commit}:.orbit"),
        ])?;
        let patch = self.git(&[
            "diff-tree",
            "--binary",
            "--full-index",
            "--no-renames",
            "--no-commit-id",
            "-r",
            &format!("{commit}^1"),
            commit,
        ])?;
        Ok(digest(format!("{orbit_tree}\0{patch}").as_bytes()))
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

        self.git(&[
            "merge-base",
            "--is-ancestor",
            &batch.through_inclusive.commit,
            &head.commit,
        ])
        .map_err(|_| AutomationError::Deferred("history_diverged".into()))?;

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
