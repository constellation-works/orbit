# Why — STD-03 Concurrency and process safety

Why each rule in [STD-03](STD-03.md) exists, tied to the incident behind it where there
was one. Read the entry for a rule when its intent is unclear or before deviating. The
rules themselves are in `STD-03.md`; this file explains them and adds none.

This section gives the reasoning and incident for each rule. The two Orbit incident write-ups
are `docs/rca/2026-09-06-recursive-test-worker-fork-storm.md` (the **09-06 fork storm**) and
`docs/rca/2026-09-23-cross-crate-test-worker-oom.md` (the **09-23 OOM**). The containment
procedure is [RB-12](../../runbooks/RB-12-cpu-fork-storm-containment.md). The 09-06 fork storm
is recorded as friction F2026-09-042, and the 09-23 OOM as F2026-09-210.

The v2 additions come from the review-corpus mining in
[tacit-extraction-2026-09-26](../../projects/standards/tacit-extraction-2026-09-26.md) (ORB-13088),
and each one cites that candidate's evidence. In evidence, `ORB-nnnnn` ids are ws_orbit tasks, `Fyyyy-mm-nnn`
ids are ws_orbit frictions, and `D:<area>:<n>` is line n of
`docs/design/<area>/4_decisions.md` in Orbit as of 2026-09-26.

**R1.** Suppose a task suspends while it holds a synchronous mutex. Every other task that
needs that mutex blocks an executor thread instead of yielding, so a small runtime deadlocks
and a large one quietly loses throughput. Orbit's `AGENTS.md` bans this, and
`[workspace.lints]` denies it. The same failure happens without any `.await` when a lock or a
transaction spans slow work (candidate K2):
- ORB-11702: companion inference ran inside the SQLite write transaction and store mutex, so
  the store's write lock was held for the length of a model call.
- ORB-11692 and D:policy-sandbox:156: the signal-handler install mutex was held for each child's
  whole lifetime, which "silently serialized every supervised subprocess" in the daemon. Orbit's
  `SignalHandlerGuard` now holds it only for the refcount and `sigaction` section.
- ORB-11614 and ORB-11615: `/api/log` and other runtime calls that lock or scan ran on async
  workers. They now go through the blocking pool. In F2026-07-119, eleven concurrent task
  updates through the API all timed out or were refused while the server was otherwise
  healthy.

A single-flight gate is the exception because serializing the slow work is its whole purpose:
Orbit's `RuntimeMemo` holds a per-key async gate across one compute so that overlapping polls
share it.
*Rust:* use `std::sync::Mutex` for short synchronous sections, `tokio::sync::Mutex` only where
the section must await, and `tokio::task::spawn_blocking` for blocking work. *Python:* never
hold a `threading.Lock` across `await`; use `asyncio.Lock`, and `asyncio.to_thread` for
blocking calls.

**R2.** An unbounded queue turns a slow consumer into memory growth. Memory exhaustion is
exactly what took the host down on 09-23, although a runaway process tree caused it rather
than a queue. Making the full-queue behaviour explicit forces the author to decide what
overload looks like. Orbit's log stream shows all three parts:
- a semaphore caps concurrent streams, and the handler refuses a new stream when no permit is
  free;
- each stream has a depth-64 channel;
- `blocking_send` backpressures the reader thread.

*Python:* `queue.Queue(maxsize=N)` and `asyncio.Queue(maxsize=N)`.

**R3.** Two locks taken in opposite orders on two paths is the classic deadlock. Orbit's task
commit boundary requires every participant to take the partition boundary lock *before* any
per-bundle lock. It makes nested acquisition on the same thread re-entrant rather than
self-deadlocking.

**R4.** This rule comes from the **09-06 fork storm**. A fixture wrote its release sentinel
only on the normal success path. When an assertion panicked, the parent timed out, or the
temp directory was dropped, the waiting child had no owner and no way to exit. Orbit's guards
show the required shape:
- `AuditGuard` starts in the `Failure` state and wraps its `Drop` side effect in
  `catch_unwind`, so a failure during cleanup cannot panic twice.
- `StagedTextFile` rolls back unless `commit()` was called.

*Rust:* mark guard types `#[must_use]`. *Python:* a context manager or a pytest fixture that
cleans up in `finally` after `yield`.

**R5.** A crash or a full disk in the middle of an in-place rewrite leaves a torn file that
the next reader cannot parse. Orbit merged three separate atomic-write helpers into one module.
It makes durable writes (rename plus parent-directory `fsync`) the default and offers a
`volatile` variant only where a caller accepts losing the write in a crash. ORB-12165 shows why
one helper matters: a side path rewrote all of `~/.claude.json` with an unlocked, non-atomic
write. Multi-file records need the same guarantee at directory level (candidate T11). In
F2026-07-097, one half-created task directory, missing its review-threads subdirectory, made
every later task read and create in the workspace fail; the friction's own remedy was "creation
should be atomic (temp dir + rename)". Orbit now stages whole task bundles in a sibling
directory and publishes them by rename. *Python:*
`tempfile.NamedTemporaryFile(dir=parent, delete=False)`, then `os.fsync`, then `os.replace`.

**R6.** A lock file that marks the lock by merely existing stays behind when its holder
crashes, and wedges every later run. A kernel advisory lock is released when the process dies.
The descriptor must be close-on-exec because a child forked while the lock is held inherits
it. ORB-12532 is a real case: a refused lock with no recorded holder turned out to be a
descriptor inherited by a child that had not yet reached `execve`.

The lock has to cover the whole read-modify-write, not just the write (candidate T11). Each of
these was a lost update or a duplicate, caused by an atomic load and an atomic save with nothing
held between them:
- ORB-12240: `--workspace` resolution saved the global registry outside `with_registry_lock`,
  so a concurrent `workspace init` registration could be lost.
- ORB-11708: a check-then-write race in `ensure_host_identity` created two machine ids.
- ORB-11100: checkoutless task updates raced lifecycle changes.
- ORB-11650: store schema migrations ran concurrently across processes until they took an
  immediate transaction and re-checked inside it.

Where no lock can span both steps, such as a remote branch, compare-and-set against the observed
version does the same job (D:task-publication:115; see R32).
*Rust:* since Rust 1.89, `std::fs::File` has `lock`, `try_lock`, `lock_shared` and `unlock`.
These are advisory locks (`flock` on Linux, `LockFileEx` on Windows) released when the file is
closed, and std opens files close-on-exec, so a new project needs no locking crate (Orbit
predates this and uses `fs2`). *Python:* `fcntl.flock(fd, fcntl.LOCK_EX)`; Python descriptors
are non-inheritable by default.

**R7.** An unbounded wait makes every command hang silently behind one stuck holder. Orbit's
default acquisition deadline is 30 seconds. The timeout error names the holder's PID, the time
it acquired the lock, and its label, so an operator can act on it. The record is written after
the lock is taken, so there is always a short window where a waiter sees the previous holder's
record or an empty one. That is acceptable because the record only explains a timeout; the
kernel lock is the only authority. Orbit treats a refusal with no readable holder as contention,
not as a free lock (ORB-12532).

**R8.** Orbit's task transition, its registry projection and its host reservation live in three
stores that cannot commit together, and a published `task.yaml` cannot be un-published.
Chaining three commits leaves a half-published fact after a crash. The boundary instead:
- records one decision, the journal row flipping to `committed` inside one SQLite transaction;
- replays everything else from that decision.

For a single store, one transaction or a pending-write guard is enough.

**R9.** The task commit boundary spells out this rule. Recovery runs before any state is
exposed. A replay that fails leaves the pending marker in place and keeps the partition closed.
"A crash after the decision leaves a repair obligation, never a successful partial
settlement." Recovery also does not infer failure from age or from a reservation expiring. A
reader who sees half-applied state, or an obligation dropped without a trace, is worse off
than one who gets a refusal.

**R10.** Before ORB-12434, every schema bump was a flag day. Older Orbit binaries still in use
included:
- stale installs on `PATH`;
- MCP servers pinned to an older release;
- workers that had inherited `ORBIT_BIN`.

Each of them either failed every command or needed out-of-band workarounds (F2026-08-063/064,
and the 2026-09-13 deploy script). The version number alone cannot tell an older binary whether
newer state is safe to read. So the newer binary records which migrations would break older
readers, and the older binary reads that record. Read-only access must be *enforced*, not
promised: Orbit sets `PRAGMA query_only=ON` on the database connection, and the store refuses
writes with a scoped error. No recorded Orbit incident came from an older binary corrupting
newer state, and this rule is why.

The compatibility claim is the weak point, so it defaults to the safe answer (candidate P2).
Orbit's decision reads: "`Additive` is a claim about binaries that *lack* the migration...
Anything that removes, renames, or reinterprets state older binaries touch is `Breaking`.
Declare `Breaking` when in doubt" (D:state-compatibility:53). A wrong "incompatible" costs an
upgrade; a wrong "compatible" lets an old binary misread live state. The enforcement at the
storage layer holds "for code that has not been written yet" (D:state-compatibility:97,
D:state-compatibility:103).

**R11.** Without a process group, an orphaned grandchild keeps the output pipes open, and the
wait on the child hangs forever (see the comment in `orbit-exec`'s spawn path, and
D:policy-sandbox:127). It also leaves nothing to signal as a unit when the child must be
stopped. *Python:* `subprocess.Popen(..., start_new_session=True)`, or `process_group=0` on
Python 3.11 and later.

**R12.** SIGTERM, then a grace period, then SIGKILL is the pattern that tini, dumb-init and
Kubernetes use. It gives well-behaved children time to flush their output and remove temp
state, and it still guarantees an end. Orbit uses a 5-second grace period (D:policy-sandbox:144,
D:activity-job:591). It polls the group with `killpg(pgid, 0)` and treats any answer other than
`ESRCH` as alive, so an uncertain probe errs toward SIGKILL. The lead child exiting says nothing
about the rest of its group. *Python:* `os.killpg(pgid, signal.SIGTERM)`, then
`proc.wait(timeout=grace)`, then `os.killpg(pgid, signal.SIGKILL)`.

**R13.** A clean exit by the child does not mean its descendants have exited. A child that is
never waited on becomes a zombie and holds a PID slot. Orbit kills the whole group even after a
clean exit, then joins its output readers (D:activity-job:591). The report matters as much as
the reaping. In F2026-07-167, a background command that died with exit 127
(`cargo: command not found`) was reported as "completed (exit code 0)", and two validation steps
were believed green while nothing had compiled. Orbit's supervisor carries `timed_out` as its
own field beside the exit code and annotates stderr with the timeout or signal, so a caller
never has to guess from text.

**R14.** After the child is reaped, the kernel may give its PID to an unrelated process. Sending
`killpg` to that number would signal processes the supervisor never spawned. Orbit registers
only a verified live group leader. Its signal handler checks again before each `killpg`, and
the waiter releases the registration as soon as the child is reaped.

The same hazard applies to every stored reference to a process, not only to signalling
(candidate K5). Orbit's decisions say "The table must never hold a bare pid"
(D:policy-sandbox:161) and "PID reuse cannot fake a live agent: a live PID whose versioned
start-identity token disagrees reads as `exited`" (D:auditability:491). Orbit's token records
the PID namespace too (ORB-10594), because a sandboxed caller in a private namespace cannot see
host PIDs at all. F2026-09-203 shows the other half of the rule. A pid-reuse hardening added a
`/proc/<pid>/stat` read to plugin callback authorization. Landlock refused that read inside the
sandbox, the failure was treated as "no matching record", and for about 50 minutes a confined
plugin could call tools outside its allowlist. A probe the subject can make fail must never
resolve to the permissive answer.

**R15.** A child that fills a pipe the parent is not reading blocks forever, and output capture
with no byte limit is one more unbounded buffer. Orbit's exec contract requires background
drain threads that start immediately after spawn. It terminates the group when the output
limit is reached. Captured text stays dangerous after the child is gone (candidate K3). In
ORB-12467, a 2.5 MB `primary_checkout_drift` diagnostic was passed whole into the recovery
agent's prompt, which then exceeded the provider's input limit, so recovery never started.
Orbit now bounds each embedded field, marks the cut, and points at the untruncated copy in the
run audit.

**R16.** In both host outages, `setsid()`-detached workers escaped every cleanup that relied on
the process group. On 09-23, the agent's `cargo test` passed in 0.78 seconds while the recursion
carried on in the background.

The unit that launches a detached worker sets that worker's fate:
- F2026-07-016: `setsid` changes the session but not the cgroup. A oneshot unit with
  `KillMode=control-group` therefore SIGKILLed workers before they could start. F2026-07-064
  records the same lesson: session detachment alone does not escape a systemd cgroup.
- 09-23: `KillMode=process` on `orbit-web.service` let a runaway tree survive every OOM kill
  and restart of the service.

Giving each worker its own bounded scope with an owner record resolves both. This is how Orbit
fixed it in ORB-12903.

**R17.** This rule also comes from the **09-06 fork storm**. The waiter was
`while [ ! -f "$SENTINEL" ]; do sleep 0.01; done`. `sleep` is a separate process, so each leaked
waiter tried about 100 process launches per second. The host had 382 leaked waiters, which
implies about 38,200 launches per second. Load average reached 393 on 14 CPUs.

An in-process wait with a deadline and a check on the child's exit status cannot multiply
this way. `docs/LESSONS.md` §3 is the source of this rule. *Python:* a loop with a deadline
that calls `time.sleep` and checks `proc.poll()`, not `subprocess.run(["sleep", ...])`.

**R18.** On 09-06, the fixture had no guard that would terminate and reap its child during
unwinding, so every failed test run leaked one more waiter. A guard that kills and waits on
drop makes a failed test leave no process behind.

**R19.** This rule comes from both incidents. When a test calls a production path that
re-executes `current_exe()`, it starts the *libtest harness*, not the product. libtest reads
the worker arguments as test-name filters, selects the spawning test again, and recurses.
F2026-08-075 recorded this trap a month before the first incident.

- **The first fix was not enough.** ORB-11418 put a test-only substitute behind `#[cfg(test)]`.
  That flag is set only when the defining crate is itself under test, so a downstream crate's
  test build compiled the guard out. F2026-09-083 is the same `#[cfg(test)]` scoping biting a
  build instead of a guard.
- **09-23:** 1,320 to 2,227 live harness processes, about 18 GiB of memory, a global OOM, and a
  hard reset.
- **The current fix, ORB-12902:** a runtime check refuses any executable shaped like a cargo
  test harness (`deps/<crate>-<16 hex>`), independent of compile flags.

Compile-time test flags are not a safety boundary.

**R20.** Running `export HOME=/tmp/x` in a shell is not isolation. Inherited authority variables
such as a managed-run context, a registry root or a workspace name take precedence over home
discovery. A fixture that relies on the shell export can then mutate the operator's real store.
Orbit's fixtures clear a shared list of inherited-authority variables on the child command.
Nextest test groups serialize the few test modules that change the process environment or the
current directory. *Python:* `monkeypatch.setenv("HOME", str(tmp_path))` in pytest, plus
passing an explicit `env=` to child processes. STD-04 builds on this rule for test isolation
in general.

**R21.** On 09-06, the run sandbox outlived its finished task for about 113 minutes, and only
cancelling the run collapsed the process tree. On 09-23, the runaway tree survived restarts of
the service. `docs/LESSONS.md` §3 states the lesson directly: "fixture cleanup and runtime
containment are separate safety layers." Either layer can fail by itself. Rules R17 to R20 make
the fixture layer good, and this rule requires the second layer anyway.

**R22.** On 09-23, the dispatching service ran with `MemoryMax=infinity` and `TasksMax=32710`,
so nothing stopped the growth before the kernel's global OOM killer froze the guest. On 09-06,
a three-hour agent wall timeout gave a leak time to compound. Orbit's worker scopes now default
to `MemoryHigh` at 40% of RAM, `MemoryMax` at 50% and `TasksMax=4096`, with `OOMPolicy=continue`,
so a breach kills a process inside the run and not the host.

The outer ceiling does not excuse unbounded waits inside the process (candidate K3). ORB-11696
added a deadline to companion RPC reads. ORB-11761 then bounded stalled
companion downloads "without restoring a whole-request deadline": a fixed ceiling on the whole
transfer fails a large download on a slow link, while an idle deadline ends only a stall. That
companion was removed on 2026-09-20; Orbit's plugin MCP backend states the surviving form: "There
is no code path that waits without a deadline." F2026-07-122 is the handle case: a synchronous
`workflow_run_resume` wedged the API while it ran, and the remedy was to return a handle.

**R23.** Candidate P2. Orbit treats its v1 baseline as "an immutable historical artifact guarded
by a structural fingerprint. Every schema change after v1 must use a new append-only migration,
and tests compare a fresh database with a v1 database upgraded through every registered version"
(D:activity-job:747). F2026-07-106 is why the parity test exists. Columns added to the v1
baseline's idempotent helpers were written as if they still ran on every open, but under the
ledger v1 runs once, so existing databases never received them while fresh databases did. Only
an appended migration reaches both. Editing a shipped migration forks the schema between
installs that ran the old text and installs that run the new one, and the ledger cannot tell
them apart. One transaction per migration, together with its ledger row, means an interrupted
migration rolls back instead of leaving half-applied schema.

**R24.** Candidate C6. In ORB-12619, a crew whose name contained `:` could be defined and used,
but putting it in a complexity pool refused every command in the workspace. The fix added a
crew-name check, and ORB-12625 found the consequence: "A config that already names a crew with
`:` is bricked by the new crew-name check, with no CLI left that can remove it", since every
`orbit config` subcommand loads the same config first. Orbit's fix is the minimum this rule
allows: the refusal names the exact file and the table to rename by hand. Where a migration is possible, prefer it. Orbit's process-identity
tokens show the grandfathered form: v1 tokens are "still read (a run claimed by an older binary
outlives its upgrade), never written".

**R25.** Candidate P6, a SHOULD because not every project ships defaults into user space. In
ORB-11745, `orbit init` without `--force` reset an operator's `executor sandbox: off`, which
silently undid a deliberate choice. ORB-10684 required retiring managed assets "without deleting
user-authored resources". The resulting decision (D:activity-job:1346) persists "the SHA-256
digest last written for each bundled activity or job"; refresh "removes retired files only when
their bytes still match that digest" and moves locally modified retired files into a backup
area. A digest separates "the tool's file, unchanged" from "the user's file", and neither a path
nor a timestamp can.

**R26.** Candidate P7, a SHOULD because the mechanism depends on how a project is installed.
F2026-07-011 found a change that reached only fresh initialization: "existing initialized
workspaces were unaffected". F2026-07-183 found a deploy that did not reach running workers:
"replacing the binary on disk does not affect already-running processes", so a feature looked
broken when it had never loaded. ORB-13013 found the opposite hazard: installing a new binary
SIGTERMed live drains and recorded them inconsistently. `orbit update` now lets the replacement
executable run migrations and managed-asset reconciliation, because only the new binary carries
the new definitions. An executable-generation record admits one executable generation per state
authority, so old and new binaries do not silently mix.

**R27.** Candidate K1. Orbit's worktree setup once published only the *name* of its start point
(`origin/<base>`), and the commit step re-resolved it. That ref is shared by every worktree off
one `.git`, so any sibling's fetch or any merge moved it, and "each new run's fetch retroactively
invalidated every older in-flight run" (D:activity-job:1020). F2026-07-113 counted 5 of 8 runs
failing on one day. The fix resolves the start point once, emits `base_sha`, and rejects a ref
name where the pinned id is expected. ORB-12655 repeated the defect in claimed-leaf validation.
The re-read half comes from F2026-07-184. The integrity guard snapshotted the task's admitted
scope before the agent was allowed to widen it, so correct in-scope changes were reported as off
scope; "the guard must re-read the task record at verify time".

**R28.** Candidate T17. Orbit's post-run integrity guard originally failed on any change to the
shared primary checkout that was not a clean fast-forward. "Every merge detonates every pipeline
run in flight at that moment" (F2026-07-101, recurring as F2026-07-139). In F2026-07-166, "a
complete, validated, test-green implementation was discarded at the gate by an unrelated
concurrent write": a sibling run had rewritten 12 learning files. In F2026-07-160, removing other
runs' files to satisfy the guard "would have destroyed their only copy". The guard now treats a
stationary primary HEAD and a proven same-branch fast-forward as benign. It leaves the real
candidate-versus-target question to the later rebase against the fetched base, where Git can
tell a clean merge from a conflict. Unexplained changes still fail closed.

**R29.** Candidate T4, with two data-loss incidents:
- F2026-09-152: `workspace teardown --confirm`, run from an unregistered checkout, resolved an
  ancestor `.orbit` and `remove_dir_all`'d another workspace's task-store partition. Twelve task
  bundles were lost, with no backup.
- ORB-12131: `--fix-orphan-task-stores` "still deletes live task bundles".

The same shape recurs in ORB-12143, which deleted a partition "whenever the bound checkout is
momentarily unreachable". ORB-12119 classified live partitions as orphaned, and ORB-12680 and
ORB-12688 had reindex delete "data-bearing partial copies". In ORB-12032, a `git reset --hard`
path was "silently destroying uncommitted work". ORB-12800's recommended command made the host
process recursively delete an unrelated directory. The decisions state the positive form:
- "On timeout, mutate only state this attempt owns" (D:activity-job:1441).
- Retire only files whose bytes still match the recorded digest (D:activity-job:1346).
- "A bundle is never easier to discard than its least-settled member" (D:activity-job:1064).

Orbit's stub reaper deletes a bundle directory only when it is provably empty residue:
"Unreadable directories are not stubs (fail closed: do not reap)." Kept data costs disk; wrongly
deleted data is gone.

**R30.** Candidate P1. In F2026-07-096, an integrity gate correctly refused a dirty worktree, but
failure cleanup then removed the worktree, and about 201 files of finished implementation were
lost: "no commit, no stash, no dangling git object". The implementation could be recovered only
from transcripts. Orbit's failure handoff now commits dirty work locally *before* pushing or
opening a PR, "so terminal runs do not strand uncommitted changes" when those fallible steps fail
(D:activity-job:1011). Integrity failures stay "byte-for-byte recoverable after forced
linked-worktree cleanup" (D:activity-job:1192). R4's rollback guards undo side effects; this
rule keeps the *product* of the work, which a rollback must not discard.

**R31.** Candidate K4. Once a request is written, the destination may have committed it, so
calling a lost reply "failed" invites a duplicate retry, and calling it "unreachable" is simply
false. Orbit's federated MCP gives a routed `tools/call` its own delivery budget from the moment
the request is written. "A lost answer after that write — budget exceeded or session ended — is
`outcome_unknown`", carrying the request id (D:federated-mcp:118). The distributed drain gives
every pull a durable request id, so "retries can recover the original admission without
consuming another task" (D:distributed-drain:258). It also refuses automatic reclamation: "TTL
expiry is not proof of death" (D:distributed-drain:47), and "age and TTL support inspection only"
(D:distributed-drain:254). F2026-08-030 is the failure this prevents: live runs were marked
`interrupted`. R9 applies the same principle inside one host.

**R32.** Candidate K6. ORB-12631: owner landing "judges containment against a remote-tracking ref
it never fetches", so a stale `origin/<branch>` stopped valid landings. A remote-tracking ref is
only as fresh as the last fetch. ORB-11110: a publication push accepted branch deletion after
observing a tip, because an ordinary push checks only the new commit, not the tip it replaces.
ORB-11525: a managed PR merge did not atomically pin the reviewed head, so a later push could
merge unreviewed. Orbit's publication decision (D:task-publication:115) reads: "Every future
publisher must compare-and-swap against the fetched branch tip. A non-fast-forward result stops
publication and surfaces an authority conflict". In Git, the compare-and-set is
`git push --force-with-lease=<ref>:<expected-sha>` (plus `--atomic` for several refs), and the
pinned merge is `gh pr merge --match-head-commit <sha>` or the merge API's `sha` field.
