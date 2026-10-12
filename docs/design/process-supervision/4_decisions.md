---
title: Process Supervision — Decisions
owner: claude
last_updated: 2026-10-12
last_validated: 2026-10-12
status: Draft
feature: process-supervision
doc_role: decisions
type: design
summary: Proposed standing rules for child-process supervision — one supervisor in orbit-common that every spawning crate reaches, new needs added as options rather than new loops, and a kill-on-drop handle owning every child from spawn to reap.
tags: [process-supervision, decisions]
paths: ["crates/orbit-common/src/process/**", "crates/orbit-exec/src/supervision/**", "crates/orbit-engine/src/activity_job/cli_runner/supervisor/**"]
related_features: [process-supervision, policy-sandbox]
related_artifacts: [ORB-15722]
---

# Process Supervision — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions remain in place so their original reasoning stays legible. See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and required `Cost:` line.

Both entries are proposed Door 2 rules. They govern the migration tasks and
any later code that spawns a child, and the operator may revise them while
this folder is `Draft`.

## One supervisor, in the lowest crate that spawns

**Recorded:** 2026-10-12 · [ORB-15722]

### Context

Orbit has three supervisors (`orbit-common`'s bounded runner,
`orbit-exec`'s `SupervisedChild` loop and the engine's provider runner) and
two ad-hoc wait loops. Each was written for one caller's needs: stdin,
capture policy, grace period, progress. Fixes for escaped pipe holders,
stopped descendants and reused pids then had to be made more than once,
and some supervisors never received them
([2_design.md §3](./2_design.md#3-capability-matrix)). The most complete
supervisor sits in `orbit-exec`, but `orbit-store` and `orbit-common`
itself spawn children and cannot depend on `orbit-exec`.

### Decision

Orbit has one child-process supervisor, in `orbit_common::process`, the
lowest crate whose code spawns children. A caller that needs something the
supervisor lacks adds it as an option or an `OutputSink`; it does not get
its own wait loop. Caller-specific policy, such as the engine's protocol
tail, the plugin build log or a provider's trace events, stays with the
caller as a sink or hook. Crates above `orbit-common` may wrap the
supervisor for their own request types (`orbit_exec::run_process`), but a
wrapper only builds options. A process that is not run to completion, such
as an MCP session, uses the shared ownership handle but not the wait loop.

The rule also covers future code. If a crate below `orbit-common` ever
spawns children, the supervisor moves down to it rather than gaining a
sibling.

The alternatives were keeping the survivor in `orbit-exec` and leaving the
bounded runner as a second, lighter supervisor for the low crates, or
adding an `orbit-store → orbit-exec` edge. The first keeps two loops with
different guarantees, which is the current problem. The second does not
help `orbit-common`'s own `fs::git` and `identity` callers.

### Consequences

- Every supervision fix lands once and reaches every caller.
- Supervision options are explicit at each call site. That is where a
  behaviour difference between callers, such as the grace period or signal
  interception, is decided and reviewed.
- Cost: `orbit-common`, the base crate, takes on the process-wide
  SIGINT/SIGTERM handler with its global slot table, a `wait-timeout`
  dependency, and about the 1,700 lines of exec's supervision module.
  Every crate links them.
- Cost: callers that needed no threads (the bounded runner on Unix) now
  start and join pipe workers on every call. The only escape is a measured
  regression justifying a second mode
  ([2_design.md §6](./2_design.md#6-concerns--honest-limitations)).

## A spawned child is owned by a kill-on-drop handle from spawn to reap

**Recorded:** 2026-10-12 · [ORB-15722]

### Context

`std::process::Child` neither kills nor waits on drop. The bounded runner
and the engine provider runner clean up with explicit kills at the exits
their authors foresaw. Their `?` early returns, and code added later
between spawn and supervision (worker registration, broker binding,
callback `bind_pid`), have skipped those kills and left the child's
process group running. `orbit-exec`'s `SupervisedChild` and the automation
source's `SourceChild` did not leak, because cleanup was in `Drop`.

### Decision

From the moment `spawn` returns until the leader is reaped, code holds a
child only through the shared owned-child handle. Its `Drop` SIGKILLs the
process group, reaps the leader and waits a bounded time for the group to
drain. Code that reaps the child marks the handle reaped, and from then on
the handle never signals that pid. A caller may borrow the inner `Child`
for setup (taking pipes, reading the pid). It never moves the `Child` out
while the process is alive. Explicit kills stay allowed as an earlier,
gentler path (SIGTERM with a grace period). The `Drop` is the backstop that
needs no foresight.

The alternative was explicit cleanup at each exit, as the engine does now.
It does no blocking work in `Drop`. But each new early return or panic is a
leak until someone notices orphaned processes.

### Consequences

- A failure anywhere between spawn and reap, including a panic, cannot
  leave a process group running.
- Cost: `Drop` blocks. It sends a signal, waits for the leader and polls
  for the group to exit for up to about 1 s, so unwinding past a live
  child is slower. A future async caller must not let the handle drop on
  an executor thread.
- Cost: every reap path must mark the handle reaped before the pid can be
  reused. A missed mark, mitigated by the own-group and group-leader checks
  before each signal, could otherwise signal an unrelated group that
  inherited the pid.

## Task References

- [ORB-15722] — proposed these rules with the process-supervision design.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
