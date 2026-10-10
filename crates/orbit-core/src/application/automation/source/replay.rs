//! Canonical history replay and frozen-batch retention.

use orbit_automation::{
    AutomationError,
    delivery::{digest, recovery::HISTORY_REPLAY_COMMIT_LIMIT},
};
use orbit_types::workflow::automation::recovery::{HistoryMapping, HistoryReplayRecord, refusal};
use orbit_types::workflow::automation::*;
use std::collections::BTreeMap;

use super::{ObservationLimits, Source};

impl<'a> Source<'a> {
    /// Prove and collect a bounded canonical replay of a diverged consumer.
    /// Provider association uses the same path as ordinary observation, but
    /// every commit in the bounded repair range must be resolved in this pass.
    pub(in crate::application::automation) fn replay_history_with_lookup(
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
        probe.lookup_retries.clear();
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
                        head: None,
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
            .observe_with_limits(
                branch,
                &probe,
                &|repository, sha| lookup(&page_source, repository, sha),
                ObservationLimits {
                    commits: HISTORY_REPLAY_COMMIT_LIMIT,
                    lookups: HISTORY_REPLAY_COMMIT_LIMIT,
                },
                Some(replay_through.clone()),
                false,
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

    pub(in crate::application::automation) fn replay_history(
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

    /// First-parent patch of each commit, in order. An empty diff is an
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
                // A merge prints nothing without a merge-diff mode. First-parent
                // is the `<commit>^1 <commit>` patch, under a single header.
                "--diff-merges=first-parent",
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
    pub(in crate::application::automation) fn retain_batch(
        &self,
        batch: &CoverageBatch,
    ) -> Result<(), AutomationError> {
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
    pub(in crate::application::automation) fn retained_refs(
        &self,
        consumer: &str,
    ) -> Result<Vec<String>, AutomationError> {
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
    pub(in crate::application::automation) fn release_refs(&self, refs: &[String]) -> Vec<String> {
        refs.iter()
            .filter(|name| self.git(&["update-ref", "-d", name]).is_err())
            .cloned()
            .collect()
    }

    pub(in crate::application::automation) fn verify_batch(
        &self,
        batch: &CoverageBatch,
    ) -> Result<(), AutomationError> {
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

    /// Every path each frozen delivery's `before..after` diff changes, keyed
    /// by delivery key: the paths its coverage evidence must account for
    /// [ORB-15186]. Renames list both sides, independent of `diff.renames`.
    pub(in crate::application::automation) fn delivery_changed_paths(
        &self,
        batch: &CoverageBatch,
    ) -> Result<BTreeMap<String, Vec<String>>, AutomationError> {
        batch
            .deliveries
            .iter()
            .map(|delivery| {
                let paths = self.git_matching_paths(
                    &[
                        "diff",
                        "--name-only",
                        "--no-renames",
                        "-z",
                        &delivery.before.commit,
                        &delivery.after.commit,
                        "--",
                    ],
                    |_| true,
                )?;
                Ok((delivery.key.clone(), paths))
            })
            .collect()
    }
}

/// Frozen-batch mismatches. Operational failures, including a prefixed
/// `source_fetch_failed`, are not mismatches: evidence evaluation defers
/// them instead of rejecting coverage.
pub(in crate::application::automation) fn is_batch_mismatch(error: &AutomationError) -> bool {
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

pub(super) fn validate_replay_range_lengths(
    orphan_count: usize,
    canonical_count: usize,
) -> Result<(), AutomationError> {
    if orphan_count == 0
        || orphan_count > HISTORY_REPLAY_COMMIT_LIMIT
        || canonical_count > HISTORY_REPLAY_COMMIT_LIMIT
    {
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
