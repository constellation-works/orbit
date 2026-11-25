# Distributed drain: setup

Contents:

- [What v1 does not do](#what-v1-does-not-do)
- [Prerequisites](#prerequisites)
- [Collapse two owners before any replica work](#collapse-two-owners-before-any-replica-work)

One owner checkout, replica checkouts on other machines, and a pull drain on
each replica. Use this when the user wants a second machine to execute the same
logical workspace, or when a claimed attempt needs inspection. Installing
matching binaries is not a rollout; starting `orbit run auto --pull` is.

The owner serves probe, receipt lookup, task admission, bind and settlement
through a deterministic internal RPC selected by Orbit's runtime SSH launch.
These five operations are absent from ordinary MCP discovery and refused by
public `tools/call`, including their formerly advertised spellings. Client names
and initialize metadata grant no internal access. Both endpoints must support
the internal protocol; preflight fails closed without a public fallback.

A
replica runs `orbit run auto --pull <selector>` to use that route. Supported
operator diagnostics remain owner-side CLI commands. Handoff approval is an
owner-dashboard action with no CLI verb or tool. Revocation and claim recovery
are supported operator commands (`orbit task revoke-handoff` and
`orbit task recover-claim`) with matching operator tools; see Recovery. Do not
invent tools for approval, write a callers file, or start a second owner store.

Owner/replica registration lives in
[multi-host.md](../../../orbit-setup/references/multi-host.md). SSH federation
and session authority live in
[remote-access-mcp.md](../../../orbit-setup/references/remote-access-mcp.md). Tool
routing is in [tool-surface-routing.md](../tool-surface-routing.md).

## What v1 does not do

- No heartbeat, automatic reclamation, fleet registry, or follower merge.
- No review on the follower's own terms. With the owner's `review.timing` at
  `before_pr`, every claimed PR leaf runs the before-PR reviewer the claim
  captured from the owner's effective pool: `workflow.purpose_crews.pre_landing_review`
  when non-empty, otherwise `workflow.purpose_crews.review`. Each follower must be
  able to run the captured crew; the follower's own `review.timing` is ignored.
  - With the
    owner's `review.timing` at `before_landing` instead, the leaf opens its PR first and that
    reviewer reviews the open PR before the leaf hands off; the owner accepts
    only the head the review settled. After-landing review
    (the owner's `delivery-code-review` auto-task) never affects admission.
    Status `review` means a delivery handoff is waiting; whether a reviewer ran
    is on the task's `review-gate.json`.
- Age, reservation TTL, and a missing local run are diagnostics, not proof of
  death.
- Remote machine labels (`--remote-caller-machine-id`) supply audit attribution,
  the receipt namespace and the machine fence for claim bind/settle. The journal
  compares the session label with the claim's execution machine, bound run and
  phase; an SSH initialize worker binding must name the same execution machine.
  The label grants no capability and is not an authenticated credential.
  - Local
    account access and SSH login are owner access in Orbit's single-user trust
    model, so this fence prevents mixed attempts among cooperating executors.
    A caller able to start a server can choose another label, including locally
    from a managed agent context; it does not isolate mutually untrusted callers.
    There is no destination callers file, forced-command acceptance, KeyBound
    proof, or replacement identity registry.

## Prerequisites

On every participating host:

```bash
orbit --version
orbit config get machine.id
orbit workspace show
orbit doctor
orbit config get review.timing                  # decisive on the owner only
orbit config get workflow.purpose_crews.pre_landing_review # owner's dedicated before-PR/before-landing pool
orbit config get workflow.purpose_crews.review  # fallback and after-landing reviewer pool
```

Require one owner per repository, matching binaries, matching distributed-drain
protocol schema `10`, and equivalent crew and toolchain resolution. Review policy
is the owner's: admission captures the owner's `review.timing` and, when it is
not `off`, its effective reviewer pool (`workflow.purpose_crews.pre_landing_review`
when non-empty, otherwise `workflow.purpose_crews.review`), review budget, and
`workflow.required_validation_commands` into the claim.

With either
timing, the owner must ship through PRs (review runs only on that route;
admission refuses otherwise) and resolve an effective reviewer pool, and every
follower must resolve and be able to run the captured crew; `orbit run auto --pull`
refuses before claiming with `before_pr_reviewer_unavailable` otherwise. The follower's own
`review.timing` is reported to the owner but never gates admission or the
leaf, so a follower need not change it.

Hosts may run different operating systems:
each follower declares its OS, and a task tagged `os:linux`, `os:macos` or
`os:windows` is claimed only by a host of a named OS; it waits in the backlog,
named, until one pulls it. Empty
`workflow.required_validation_commands` means no host-required validation check:
a claimed leaf runs no such check, and its review certificate records the
captured empty list.

When configured, the owner requires the reviewer to report
each captured command as passing evidence, then independently verifies the
owner's exact-run and exact-head handoff logs against the same command list.
List skew or missing evidence fails closed.
Every admission receipt also captures the owner's
`workflow.required_validation_commands` and `review.baseline_commands`, review or not.

A claimed leaf validates, replays sandbox deferrals and trusts baseline reruns
against that capture, not against the follower's own lists. A refusal names
the gate list it checked and its source. A claim from an older owner that
captured no gates falls back to the follower's lists with a warning.
Deny a caller by removing its key from `~/.ssh/authorized_keys`.

Per-host compiler capacity is independent of drain slots. Keep the shared
build-budget defaults (two heavy slots, four Cargo jobs) unless the operator
raises them.

Managed agents never propagate `--operator` or `ORBIT_OPERATOR`. Claim,
machine, bound run, and phase checks fence attempt ownership from runtime/session
invocation context, not tool arguments. For remote bind/settle, that context's
machine comes from the caller-chosen session label described above.

## Collapse two owners before any replica work

If both hosts initialized the same repo as owners, stop competing drains on the
host that will become a replica, reconcile in-flight runs and reservations,
export tasks that must move, then re-register. An owner checkout is never
rebound in place, so drop its registration first (registry only; `.orbit` stays):

```bash
ORBIT_OPERATOR=1 orbit workspace remove <workspace-id>
orbit workspace init --role replica --owner <owner-machine-id>
orbit workspace show
orbit workspace role <workspace-id> replica --owner <owner-machine-id>
```

`workspace role` reasserts; it is not a takeover. Copying Git history does not
move the control plane.

```bash
orbit task export --ids <task-id> -o <archive>
orbit task import <archive> --on-conflict=renumber
```

`--on-conflict=owner-wins` is for a repeatable mirror of IDs the owner already
minted. Do not rsync the store. Restore pruned `context_files` only from
history:

```bash
orbit task lint <task-id>
orbit task lint <task-id> --restore-pruned
```

Missing files are valid declarations. `--restore-pruned` never guesses.
Reservation TTL on a pulled claim is 14,400 seconds and does not revoke the
claim or shrink the frozen footprint.

A claimed implementer may add files anywhere the work requires; the frozen
footprint is a scheduling hint, not a delivery gate. Every added path the
footprint does not cover becomes a widening request, derived from the final Git
candidate rather than the implementer's reported selectors. The owner
independently checks the diff and accepts the widening even when another live
claim, in-progress/review selector or reservation names the path.

Acceptance
records exact file selectors, a `context_files_widened` history entry and the
enlarged live claim; the original receipt stays immutable. Only Git or `.orbit`
metadata, environment files, symlinks and malformed paths are refused, with
exact paths. Protected metadata names and environment patterns (including `.envrc`)
ignore ASCII case on every host: `.Orbit/`, `.GIT/`, `.ENV` and `.Env.local`
are refused even on Linux.

Both peers require the same protocol revision (currently 10).
Tagged `no-diff-expected` tasks are claimable and hand off `NoDiff` with no
PR; a review needs no report and runs no required validation or replay, which
the owner accepts while its copy of the task keeps the tag, and a tagged claim
that changes code is refused as `no_diff_expected_changed`.

An ordinary claim that proves its
implementation changes nothing may also hand off `NoDiff`. Unless it returns the
`already_resolved` outcome below, its implementer writes the verifier report
and its logs beneath `.orbit/tmp/` and returns `no_diff_artifacts` scratch
references; commit imports and verifies them through the claim.

Required
ledgers, screenshots and other task artifacts use a separate
`task_artifacts: [{path, source_path}]` output for diff and no-diff candidates,
including `no-diff-expected` tasks. Keep sources beneath the leaf's `.orbit/tmp/`
and each file at most 1 MiB. Commit validates all files before attachment and
refuses missing or oversized files, scratch escapes and invalid or duplicate
artifact paths, naming the path.

Task artifacts cannot use either clean-tree
report name or a report's declared log names, and cannot use the reserved
`review-` namespace (any ASCII case), which only the review gate writes;
`no_diff_artifacts` remains limited to the report and its logs. Owner mode
uses `orbit.task.artifact` action `put`.
The owner rechecks the report against its live base before authorized completion
without a PR. A changed base or evidence requires fresh validation.

The report-free alternative is the `already_resolved` outcome. When the tests that
show the base resolves the task run only outside the agent sandbox, the
implementer may return `already_resolved`
`{"selectors": ["cargo test -p <crate> --test <target> [<filter>]"]}` or
`{"selectors": ["cargo test -p <crate> --lib [<filter>]"]}` with its
execution summary and no `no_diff_artifacts`.

Commit settles that as
`verified_already_resolved`, embedding the selectors and summary, when neither
clean-tree report artifact exists, and the leaf hands off `NoDiff` without a PR.
The claim host then runs the required commands on the base and replays each
selector outside the agent sandbox before handoff, pinning the passing logs.
When the implementer declares `already_resolved` selectors beside a verifier
report, commit leaves the report-backed `verified_no_diff` or
`verified_already_landed` checkpoint without selector evidence; claim
validation records the admitted selectors in the pinned `NoDiff` handoff
artifact instead, then replays each selector the same way before handoff. On a
`no-diff-expected` skip the claim host omits selector evidence and runs no
replay, while owner settlement refuses a submitted skip checkpoint that still
carries selector evidence.

A
failing replay fails the claim, as does one that runs no test or prints a skip;
an empty selector list, more than 16 selectors, or a selector outside that
grammar is refused before anything runs. A candidate that also changes the base
is refused for this outcome. The owner accepts the checkpoint only when it
names this task and its pinned base and its selectors still admit.

Artifact-backed evidence stays authoritative. A verifier report or already-landed
report that the implementer supplies is imported and verified first, and a
malformed or failed report is refused rather than replaced by selectors. A
summary alone, with neither a report nor selectors, is refused.

A handoff may
also carry `owed_criteria`:
criteria the owner's pilot typed `post_deploy`, which only the deployed build
can evidence. The owner refuses the settlement when its own pilot finding does
not type each one, and otherwise lands the handoff with the task left in
`review` until an operator records them (see the task-execution reference).


A claimed PR leaf that fails before its normal commit preserves unfinished
implementer changes as one marked partial commit, titled `Partial implementation
candidate [<task>]`. Its branch, index and files stay unchanged. The leaf then
pushes that commit to `refs/orbit/candidates/<task>/<run>` on `origin`.

A pushed
commit is durable: the next claim on any host fetches and applies it through
`resume_candidate` with trigger `implementation`, so the implementer finishes the
existing work. Settlement records the durable ref in task history.

If `origin` refuses the push, the leaf keeps the partial commit and the push
diagnostic, but there is no durable ref. The commit is host-local: only the host
that holds it can resume it, and only while that host's checkout still has the
commit. Pushing the commit to `origin` by hand does not make it resumable
elsewhere, because placement reads the recorded durable ref.

To find where the commit is, run `orbit task show <task-id>` and read the owner's
latest `claim_candidate_kept` history entry. It names the kept head, and its note
reads either `durable at <ref>` or `only on <machine> (pushing it failed:
<diagnostic>)`, followed by `stopped at <step>`. To have the work resumed, run the
follower's drain on the machine named after `only on`, from its replica checkout
(see [the follower's drain section](distributed-drain-follower.md#start-a-followers-drain)).

Its claim of this task resumes the
commit on that host. A claim on any other host implements the task afresh from
the base. The owner's history then records a `candidate_resume` entry whose note
starts `fresh:` and reads `reason=not_durable` with the kept head, and the holding
host is still offered the commit. Route the drain to the holding host when the
commit's work should continue.

Protected paths, environment files, symlinks and gitlinks refuse the entire
snapshot with a settlement diagnostic. Nothing is kept in that case, so the owner
writes no `claim_candidate_kept` entry, and the reason reaches the owner in the
claim's release failure. `.orbit/tmp/` scratch is excluded.
