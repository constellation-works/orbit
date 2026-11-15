---
id: STD-03-concurrency-and-process-safety
title: Concurrency and process safety — locks, channels, durable writes, state evolution, subprocess lifecycles, destructive operations, remote effects, test containment and resource bounds
summary: Normative rules for async locking, bounded queues and waits, cleanup guards, atomic and locked durable writes, version skew and append-only migrations, subprocess supervision and process identity, pinned inputs and interference-scoped guards, ownership-proven destruction, outcome-unknown remote calls, test-fixture safety and resource ceilings; follow it in any project that runs concurrent code, spawns processes, talks to peers, or persists state.
status: active
tags: [standard, concurrency, subprocess, testing, durability, migrations, git, rust, python]
version: 2
created: 2026-09-26
updated: 2026-09-26
last_validated: 2026-09-26
related: [STD-01-cli-surface, STD-02-rust-architecture-and-errors, STD-04-testing-and-verification, STD-05-security-boundaries]
---

# Concurrency and process safety

These rules cover shared state, async locking, durable writes and how that state evolves, child
processes, destructive and remote operations, and the tests that exercise them. Most of them
come from failures the Orbit codebase has already had. Two of those failures took down the host
that runs the constellation: the 2026-09-06 fork storm and the 2026-09-23 out-of-memory outage.
The principles hold in any language. Rust specifics (lints, `Drop`, `std::process`) and Python
equivalents appear where they apply. Orbit is the worked example here, not the audience. Orbit
paths are relative to `codebases/orbit/`.

The keywords MUST, MUST NOT and SHOULD are normative. A rule marked *review-only* in
[`checks.md`](checks.md) has no automated gate, so a reviewer enforces it. Rule numbers are stable: v2
added R23 to R32 inside their clusters, so numbers do not run in order down the page.

This standard is a directory. This file holds the binding text: the rules, where they
apply, and how to deviate. Beside it, [`why.md`](why.md) gives the reason for each rule,
[`checks.md`](checks.md) the gate that enforces it, [`exemplars.md`](exemplars.md) worked
examples in Orbit, and [`CHANGELOG.md`](CHANGELOG.md) what changed in each version.

## Rules

### Async and in-memory shared state

- **R1.** A synchronous lock guard (a mutex, a read-write lock, or a `RefCell` borrow) MUST NOT
  be held across an `.await` or any other suspension point. Copy the data out or end the
  guard's scope first. Where the critical section itself must await, use an async-aware lock.
  A lock of either kind, or an open database transaction, MUST NOT be held across slow work: a
  subprocess, network I/O, model inference, or a scan of a large file or directory. The one
  exception is a lock whose only job is to serialize that work, such as a per-key single-flight
  gate that nothing else waits on. Blocking calls MUST NOT run on an async executor thread;
  hand them to a blocking pool.
- **R2.** Every channel or queue between concurrent tasks or threads MUST be bounded. The
  construction site MUST state what a producer does when the queue is full: wait, which applies
  backpressure; drop with a counted signal; or refuse the request.
- **R3.** Code that holds more than one lock at a time MUST acquire the locks in one documented
  order, the same order on every path.

### Cleanup guards

- **R4.** Every side effect that must be undone MUST be bound to a guard. Examples are a held
  lock, a temp file, a changed signal disposition, an environment override, a spawned child, or
  a pending-write marker. The guard's destructor or `finally` block runs on success, error
  return, panic or exception, and cancellation or timeout. It defaults to the failure or
  rollback outcome, and it never panics or throws.

### Durable on-disk state

- **R5.** A durable file MUST be replaced atomically. Write a temp file in the same directory,
  flush it (`fsync`), rename it over the target, and `fsync` the parent directory when the write
  must survive a crash. A durable file is never truncated and rewritten in place. Every durable
  write in a codebase goes through one shared helper, so the flush and rename steps cannot drift
  between copies. A record that spans several files is staged complete in a temp directory
  beside its final location and renamed into place, so no reader sees a partial record.
- **R6.** On-disk state that more than one process mutates MUST be changed only while holding
  an OS advisory lock that the kernel releases when the holder dies: `flock`, `fcntl` or
  `LockFileEx`. Open the lock file close-on-exec. Never use a create-if-absent lock file or a
  PID file as the lock. A read-modify-write of shared durable state (a registry, a config file,
  a schema) MUST hold the owning lock from before the read until after the write, or MUST
  commit with a compare-and-set against the version it read. An atomic load followed by an
  atomic save is not an atomic update.
- **R7.** Lock acquisition MUST be bounded by a deadline. A timeout MUST report who holds the
  lock: the holder records its PID, the time it acquired the lock, and a label. The holder
  record is diagnostic only. The holder writes it just after acquiring, so a waiter can briefly
  read the previous holder's record, or none, and nothing decides ownership from it.
- **R8.** Writes that must become visible together across more than one store MUST publish
  through one durable commit decision: a single transaction or journal record. Every other
  effect replays idempotently from that decision. Never chain independently committed writes.
- **R9.** Recovery of an interrupted multi-step write MUST finish before any state is exposed:
  it either completes the write or refuses and keeps the store closed. It MUST NOT drop an
  obligation that was already decided, and it MUST NOT guess the outcome from age or a timeout.

### Version skew and state evolution

- **R10.** A binary that finds persisted state newer than it understands MUST NOT write to it,
  migrate it down, or reinterpret it. The binary has two options:
  - If the newer writer declared the changes compatible, open the state read-only, with writes
    refused at the storage layer.
  - Otherwise, refuse to open it, and name the first incompatible migration it lacks.

  A writer declares a migration compatible only when binaries that lack it still read the state
  correctly. A change that removes, renames or reinterprets state that older binaries touch is
  incompatible, and so is any change whose compatibility is in doubt.
- **R23.** Every schema or on-disk layout change MUST ship as a new migration appended to an
  ordered, versioned registry. A shipped migration is never edited, reordered or removed, and
  the baseline schema is frozen once released. Each migration applies in one transaction
  together with its ledger record. A store created fresh and a store upgraded from the oldest
  supported version MUST end with the same schema.
- **R24.** A new or stricter validation rule MUST migrate or grandfather existing state that
  the old rule accepted: a config file, a persisted record, an identifier an earlier release
  minted. An old format stays readable even when it is no longer written. Where neither is
  possible, the refusal names the file or record and a repair that works without the tool,
  because a tool that refuses to load the state cannot be used to fix it.
- **R25.** *(SHOULD)* A tool that installs default resources (config, templates, job
  definitions) into user-owned locations SHOULD reconcile them by provenance. It records a
  digest of what it last wrote, and it refreshes or retires a file only while the file's bytes
  still match that digest. A user-authored or user-modified file is preserved; a modified file
  that a release retires is moved aside, not deleted. Re-running init without a force flag
  SHOULD NOT reset a setting the user changed.
- **R26.** *(SHOULD)* A fix to persisted state, installed resources or runtime behaviour SHOULD
  reach installs that already exist and processes that are already running, not only a fresh
  init. The upgrade path runs the *new* binary's migrations and resource reconciliation against
  existing state. Long-lived processes are drained and restarted onto the new binary, or they
  stay visibly marked as running the superseded one. Replacing the binary on disk is not a
  deploy.

### Subprocesses

- **R11.** Every spawned child MUST be the leader of its own process group, or be placed in its
  own cgroup or job object, so that the child and everything it spawns can be signalled as one
  unit.
- **R12.** Termination on timeout, cancellation or a parent signal MUST follow three steps. Send
  a polite signal (SIGTERM) to the whole group. Wait a bounded grace period. SIGKILL the group
  if anything survives. Survival is decided by probing the group itself, and a probe that cannot
  answer counts as survival. It is never inferred from the lead child's exit.
- **R13.** Every exit path, including a clean exit, MUST end with the child reaped and its group
  swept before the supervisor returns. Sweeping means SIGKILL to any surviving member. The
  supervisor reports how the child actually ended: its exit code, or the signal or timeout that
  ended it. A child that failed to start, timed out or was killed is never reported as a clean
  exit.
- **R14.** A supervisor MUST NOT signal a PID or process group after the child that owned it has
  been reaped. It signals as a group only a live group leader that it spawned itself. Any
  persisted or cross-process reference to a process MUST identify it by PID together with its
  start identity (a start-time token, plus the PID namespace where that can differ), never by a
  bare PID. A live PID with a different start identity reads as exited. A probe that cannot
  answer reads as unknown. Unknown is never proof that the process is gone, and never proof of
  identity where the answer grants authority.
- **R15.** A child's stdout and stderr MUST be drained at the same time as the wait, and
  captured output MUST have a byte limit. When the limit is reached, the child is terminated or
  its output truncated. The same limit applies to captured text passed onward, such as an error
  message or a peer's reply embedded in a record or a prompt: it is bounded, the truncation is
  marked, and the full text is kept somewhere durable.
- **R16.** A deliberately detached process MUST satisfy two conditions: a durable owner record
  identifies it, and it runs in its own resource-bounded container (a cgroup scope, unit or job
  object). A detached process is one started with `setsid` or daemonized, so it outlives its
  spawner.

### Tests and fixtures

- **R17.** A test that waits for a concurrent process MUST wait in-process, with an explicit
  deadline and a liveness check on the child. It MUST NOT use a shell loop that forks a process
  on every iteration, and MUST NOT wait without a bound.
- **R18.** Every test fixture that spawns a process MUST own it through a panic-safe guard that
  terminates and reaps it on drop.
- **R19.** Tests MUST NOT run the real product binary or worker through a production
  self-re-exec path (`current_exe()`, `sys.argv[0]`). The spawn path requires an explicit test
  substitute. It also refuses at runtime when the executable is a test harness, whatever
  compile-time test flags are set.
- **R20.** Tests MUST NOT read or write real host state. Every fixture runs against temporary
  roots. `HOME` and every product-specific root or authority variable are set or cleared on the
  child command, not only in a parent shell. Tests that change process-global state (the
  environment, the current directory) are serialized under one process-wide guard or test
  group.

### Containment and resource bounds

- **R21.** The runtime that hosts test or agent workloads MUST reclaim every descendant when
  the owning unit ends, whether or not in-process cleanup ran. That runtime is the CI job, the
  sandbox, or the pipeline run. Fixture cleanup and runtime containment are separate safety
  layers, and neither may rely on the other.
- **R22.** Anything that runs unattended MUST have a wall-clock timeout and a process-count and
  memory ceiling, enforced outside the process itself. That covers services, timer jobs,
  workers, CI jobs and test runs. Inside the process, every wait on a peer, a child, a lock or a
  stream MUST also have a deadline, including joins on output readers after a timeout. A
  streaming read or long transfer uses an idle deadline that resets on progress, not a single
  deadline for the whole request, so a slow live transfer finishes and a stalled one ends. An
  operation longer than its caller can wait SHOULD return a handle to poll instead of blocking
  the caller.

### Moving inputs and integrity guards

- **R27.** A long-running operation MUST resolve each moving reference it depends on (a branch
  name, a `latest` tag, a symlink, a pointer in config) to an immutable identifier once, at
  start. It records that identifier and passes it to every later step, and no later step
  re-resolves the name. An input that the operation's contract allows to change while it runs,
  such as a scope the operation may widen, is re-read at verify time rather than frozen at
  start.
- **R28.** A fail-closed concurrency or integrity guard MUST compare only state that can affect
  the operation it protects, such as overlap with the operation's own paths or records. It MUST
  NOT demand that unrelated shared state stay byte-identical, because then every unrelated
  concurrent write fails the guard. A change the guard cannot classify still fails closed, and
  a guard failure preserves the work it stopped (R30).

### Destructive operations and recovery

- **R29.** Before deleting, resetting, overwriting or reclaiming anything, code MUST establish
  positively that it owns the target and that the target holds nothing unrecoverable. Ownership
  comes from recorded provenance: a digest of what this code wrote, an owner record, or a
  registration this attempt created. It never comes from a location, a name, or the absence of
  another claim. A target that is unreachable, unreadable, ambiguous or only partly read is
  kept and reported, never deleted. On timeout or failure, code mutates only state that this
  attempt owns.
- **R30.** A failure path MUST make completed work durable and recoverable before any teardown.
  It commits or snapshots the work and records where it is, and only then removes the worktree,
  temp directory or sandbox. The preservation step runs before the fallible operations (a push,
  a network call, a gate) that might end the run, not after them.

### Distributed and remote effects

- **R31.** Once a request that can change remote state has been sent, a lost or timed-out reply
  MUST be reported as outcome unknown: not as failure, and not as the destination being
  unreachable. A retry MUST carry an idempotency key or request id so the destination can
  recognize the repeat. A silent or unreachable peer is not a dead one: work it holds is never
  reclaimed automatically because a TTL or heartbeat lapsed. Age supports inspection and an
  explicit, recorded reclaim.
- **R32.** Git automation MUST fetch the relevant refs before judging containment, ancestry or
  freshness against a remote. It MUST update a shared remote ref with an expected-old-value
  compare-and-set, and it merges reviewed work only at the reviewed head commit. A ref that
  moved is a conflict to surface. It is never merged, replayed or force-pushed over
  automatically.

## Applies to

- `universal`: R1 to R4, R11 to R22, R27 and R28. Any project with concurrency, child
  processes or tests.
- `universal`, where the project does the thing: R29 and R30 for any code that deletes,
  resets or tears down; R31 for any code that sends requests to another process or host; R32
  for any code that automates Git.
- `stateful`: R5 to R10, and R23 to R26 (R25 and R26 are SHOULD). Any project that persists
  state across runs or processes.
- `service`: R16, R22 and R26 bind hardest on long-running daemons, workers and timer jobs.
- `rust`: the Rust gates in [`checks.md`](checks.md).

## Deviations

An adopting repo that departs from a rule records a decision in its own design docs, citing
`STD-03@2 §Rn` and the reason. It never edits its vendored copy of this standard.

Some deviations are expected, and the decision still needs to be recorded:
- A platform without process groups (Windows) meets R11 to R14 with job objects, citing
  `STD-03@2 §R11`.
- A host without a cgroup manager meets R16 and R22 with `setrlimit` (`RLIMIT_NPROC`) plus a
  loud warning, citing `STD-03@2 §R16`. Orbit's own fallback runs the worker in the caller's
  cgroup and warns once per process.
