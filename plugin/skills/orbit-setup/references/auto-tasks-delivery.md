# Auto-tasks: delivery consumers

## Delivery consumers: stalls, automatic replay, and reset

A definition scheduled on `deliveries_landed` (the shipped
`delivery-code-review` definition) is not on a clock: it watches a branch, records
which landings it has examined, and mints an examination task when the debt is
due. That recorded position — "observed commit X" — is what a rewritten branch
history breaks.

When the checkout has an `origin` remote, the consumer observes
`origin/<branch>`, not the local branch. Each pass fetches that one ref and
leaves the worktree, index, and local branch untouched, so a pull request
merged on the remote is seen even when the checkout was not fast-forwarded. A
failed fetch defers as `source_fetch_failed` and does not fall back to the
local ref; that deferral retries.

`orbit doctor`'s `review` row names the
observed commit and the remote-tracking head, and it is not ok while the cursor
trails that head past the batch's `max_wait_minutes`. A repository with no
remote still watches `refs/heads/<branch>`. Doctor compares the remote-tracking
ref already in the checkout and does not itself fetch.

### A rewritten history is proved, or it stalls

When the observed commit is no longer reachable from the configured branch head,
the evaluator runs a replay proof: every observed commit must map onto a commit
on the new head with the same `.orbit` tree and the same parent-relative patch.

- **Proof succeeds** (a rebase or amend that preserved content) — the consumer
  reconciles itself in that tick, under an audit record attributed to
  `system:automation`, keeping every obligation. One friction is filed so the
  rewrite is visible, and the next tick observes normally. Nothing to do.
- **Proof fails** (a commit was dropped, squashed away, or its debt can no
  longer be identified) — the consumer **stalls**: evaluation suspends, one
  friction is filed listing the obligations that could not be mapped, and the
  tick prints a single `stalled: history_diverged` line instead of an error
  every 60 seconds. Nothing resumes it without an operator.

Frictions are deduped per rewrite, so repeated ticks and the sibling consumer on
the same branch share one record rather than filing hundreds.

Find stalled consumers with `orbit doctor` (the `automation-consumers` row) or
`orbit auto-task recover <name>`, whose stall reason names the cause. A stall
that persists past 60 minutes is also logged
at `warn`, so `orbit log tail --level warn` shows it.

### Clearing a stall

Two commands clear it, and both write an audit record:

```bash
# Retain every obligation: reconcile a rewrite the proof can verify.
orbit auto-task recover delivery-code-review --replay-history          # preview
orbit auto-task recover delivery-code-review --replay-history --reason "<why>"

# Forget the debt: re-baseline at the current branch head.
orbit auto-task reset delivery-code-review                            # preview
orbit auto-task reset delivery-code-review --reason "<why>"
```

Prefer `recover`. Use `reset` when no recovery can repair the consumer — the
rewrite destroyed the debt's identity, or the state predates the recorded
trigger and `recover` refuses it as `coverage_unverifiable`.

### What `reset` does

`reset` is the only Orbit operation that discards coverage debt, so it previews
until it carries `--reason`. The preview prints the consumer key, its
generation, everything that would be forgotten (pending deliveries and commits,
unresolved evidence, waived and excluded landings, accepted receipts), any
executing action, and the head it would re-baseline at. Read it before
authorizing.

An apply writes one `reset` recovery record (who, when, why, the previous
epoch/generation, the forgotten inventory, the new baseline), drops the consumer
state, and deletes the `refs/orbit/automation/<digest>/*` pins of the forgotten
batches. The next tick reports `baselined` at the branch head. Landings before
that baseline are never examined — that is the cost of the command, and the
record is the only trace they existed.

It refuses while an action is executing unless `--force` is passed (the admitted
task is abandoned, not cancelled), and refuses a state-member consumer. Never
hand-edit the `automation_consumers` row instead: that leaves no audit record.

Automation frictions are tagged `automation`, plus `history-diverged` for a
rewritten history. Those tags ship in the default vocabulary; loading the
taxonomy merges any missing defaults into an existing `.orbit/frictions/tags.yaml`
without removing operator-added tags.
