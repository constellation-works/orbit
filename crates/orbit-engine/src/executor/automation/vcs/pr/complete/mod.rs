//! Completion-authorized PR delivery [ORB-11187].
//!
//! `pr_promote` ends a PR-mode run at `review`. When the operator granted this
//! invocation completion authority (`--complete`), this step carries the same
//! PR the rest of the pipeline opened the rest of the way: it drives the merge
//! through GitHub's own gates, verifies the merge actually happened, and only
//! then runs the guarded `review -> done` transition.
//!
//! The invariant this module exists to hold is that *merged* is established by
//! reading GitHub's merged state back, never inferred from having asked. In
//! particular, enabling auto-merge is not terminal success: the poll continues
//! until the PR reports `MERGED`, or the wait budget expires and the run fails
//! with the tasks still in `review`.
//!
//! Branch protection is respected by construction. The merge request is an
//! ordinary provider merge (ungated runs may use `--auto`); no administrative
//! bypass is reachable. A `BLOCKED` PR with checks still running and no
//! outstanding review is polled within the wait budget; failed checks,
//! required reviews, and unreadable check results refuse completion.
//!
//! A `DIRTY` PR is narrower than those policy refusals. With the pipeline's
//! retained `completion: done`, published-head, branch, and base checkpoints,
//! completion reuses the ordinary pinned `git_rebase` and lease-checked push
//! boundaries. Only a rebase that proves unmerged index entries can reach the
//! existing bounded conflict-recovery leaf. The retry after that recovery
//! re-pins to the base it fetches; when the base advanced past the certified
//! recovery meanwhile, `git_rebase` chases it within a bound instead of
//! refusing the recovered head [ORB-14393].
//!
//! [ORB-11982] tightens what "merged" is allowed to mean. The same checkpoints
//! also pin *which* pull request this run may deliver, so a base-modification
//! race that repoints the PR, a head this run never published, or a merged
//! state reported without a merge commit all refuse completion instead of
//! reaching the transition. See [`super::delivery`].
//!
//! [ORB-13890] A `DIRTY` PR whose head a before-PR review certified cannot be
//! merged as rebased, unreviewed content. With `re_review_on_conflict` and the
//! same checkpoints, completion rebases the branch locally through the same
//! pinned `git_rebase` (a real conflict still reaches conflict recovery),
//! publishes nothing, and returns `re_review_required` with the rebase
//! checkpoint; the pipeline then reviews, republishes and completes the new
//! head. Without the flag it stays `review_gate_stale`; the pipeline sets it
//! on every completion round but its last, so a base that keeps moving under
//! re-review ends in review rather than in an unbounded loop [ORB-14332].
//!
//! [ORB-14849] A before-landing review settles the published PR head
//! instead, after `pr_open`. Its `landing_reviewed_head_sha` then decides:
//! completion merges only that head, and when the reviewer's fix moved the
//! PR there, that head is the published one and the head it replaced the
//! previous one. The DIRTY rule above applies to it unchanged.
//!
//! [ORB-13444] Ungated completion is held to that same candidate. The
//! published head is checked before any merge or auto-merge request, and that
//! SHA is what the synchronous provider mutation requires. Pending work that
//! carries a published head waits for the mutation instead of enabling
//! auto-merge, which cannot keep the condition. A run with no published SHA
//! still uses the ungated merge.
//!
//! [ORB-14205] GitHub refuses that synchronous mutation with "Base branch was
//! modified" (HTTP 405) when another merge lands on the base between its
//! mergeability check and the request. That refusal alone is retried: after
//! the ordinary poll wait, completion re-reads the PR, re-applies every pin
//! and gate above, re-resolves the merge policy, and asks again with the same
//! SHA. At most `BASE_MODIFIED_MERGE_ATTEMPTS` requests are made, within the
//! unchanged wait budget.
//!
//! `auto_fix: true` (`orbit run auto --auto-fix`) authorizes repair of failed
//! required checks and merge conflicts on this pull request (see
//! [`super::auto_fix`]). Without `completion: done` beside it the step only
//! watches: it never requests a merge or completes a task, and ends once the
//! pull request is mergeable, leaving the task in review.

mod conflict;
mod drive;
mod mutation;
mod reviewed;
mod run;

pub(in crate::executor::automation) use run::pr_complete;
