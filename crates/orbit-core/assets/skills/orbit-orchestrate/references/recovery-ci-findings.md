# Route fresh CI and QA findings into repairs

## CI and QA: verify freshness before filing

Use `ci_failure_sweep_pipeline` for CI discovery. Its current admission path
files proposed repairs, runs pilot, and can promote its own warning-free,
selector-backed tasks; it does not implement them. Inspect the resulting task
and admission evidence rather than duplicating that work. See
[workflows-ci-sweep.md](workflows-ci-sweep.md).

Before filing or dispatching a repair, compare the failing run/job and SHA
with the current landing branch and prior fix PRs. An old CI failure can arrive
after the repair merged; the sweep holds such failures in
`pending_supersession` rather than filing them, so read that list in the
sweep's step output before filing by hand.

A red run is held there for a newer
push run still in flight at a descendant commit only while it is a lone
failure: once the previous completed run failed the same job with the same
normalized error signature, the sweep files it (`reproduced_on`), and a hold older than
`pending_supersession_window_minutes` (default 30) is filed with the pending
run named in the task (`held_past_window`).

A failure that sits in
`pending_supersession` across sweeps on a busy branch is therefore not
waiting on you; one that never gets filed is a sweep defect. Attach the exact failing command/log excerpt and
current reproducibility evidence to one bounded task. Search open and closed
history; reject proven duplicates with a link to the delivered fix. Cancel a
duplicate's active child only within authorization and after inspecting its
state; do not cancel the whole drain.

When a hand fix is open, archive the sweep finding with a `covered_by`
relation in the same task update. Preserve all existing relations and append
`{"type":"covered_by","target":"<covering task id>"}` or
`{"type":"covered_by","target":"github-pr:https://github.com/OWNER/REPO/pull/NUMBER"}`.
`github-pr:NUMBER` uses the checkout's GitHub repository.

This is durable
operator coverage of the evidenced cause: later sweeps report `covered`
with the archived owner and cover, and create no pilot candidate. A compiler
error's key includes the checkout, so the hold also follows the same complete
diagnostic set (paths and messages, ignoring line and column) to later
checkouts; an owner filed before that set was recorded in its description
holds only at its own checkout. A rejected finding can carry the same
relation.

A missing or unreadable cover remains withheld with
`operator_cover_unavailable`; inspect that reason rather than assuming the PR
is open. A closed, unmerged PR (or archived/rejected cover task)
releases the key. After a merged cover's commit is in the failing checkout,
the sweep files a new repair and names that cover as a fix that did not hold;
older checkouts stay covered while waiting for the fix.

A plain archive or rejection without `covered_by` suppresses an equivalent
evidenced cause (and, for compiler errors, the same diagnostic set at later
checkouts) for a fixed 6 hours from the
recorded status decision. A historical job/step key alone does not establish
equivalence: a different retained diagnostic can file a new repair.

When the
cause cannot be established, `archive_identity_unavailable` holds filing
within the same window; a finding with a cover stays held for reconciliation
without attributing the uncertain cause to that cover.
Such entries name only a `candidate_owner`, with no confirmed `owner` or
`cover`; a hold for cover reconciliation has no expiry.
The report entry is `withheld`, with the owner, expiry and `archive_actor`.

Its reason is `operator_archived` for a human status decision,
`automation_archived` for automation (including task-pilot's automatic
`verified_no_diff` archival), or `archive_provenance_unavailable` when no
status actor was retained. Later metadata edits do not change that actor or
restart the window. After expiry a current failure can file again.

Matching normalized failing-test signatures across jobs share one task with
all job names and source identities; different signatures remain separate.
Read `file.withheld` and its matching audit entries alongside
`skipped_existing` and `pending_supersession` when assessing CI relief.

Post-merge code review and QA follow the same loop. Exercise real user paths
and report concrete defects; do not replace verification with an agent's
claim that the change is correct. Pilot and promptly promote authorized
repairs while independent work continues.

Run an authorized sweep on the owning workspace with:

```bash
orbit run job ci_failure_sweep_pipeline
```

Check whether its routine or a manual invocation is already active first.
