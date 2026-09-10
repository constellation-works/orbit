---
type: design
summary: "Spec: Sandboxed Exec Contract"
tags: ["policy-sandbox"]
last_validated: 2026-09-08
---

# Spec: Sandboxed Exec Contract

`orbit-exec::run_process` is the common validated-spawn primitive. Platform sandbox wrappers can instead create a child and pass it to `supervise_child`, which shares the supervision implementation. This spec names the invariants and failure modes those paths must preserve.

## Why This Exists

Process supervision is full of subtle deadlocks (full pipe buffers, orphan grandchildren, signal races). Without a prescriptive contract, callers may build tools that bypass the supervision layer or assume invariants that the layer does not actually provide.

## Spawn Invariants

- **Sandbox validation first.** `run_process` calls `sandbox.validate(req)` before spawning. The default `NoSandbox` always returns `Ok`, but any future impl that returns `Err` aborts the spawn before any state changes.
- **Pipes for capture.** Stdout and stderr are always piped to the parent. Tools that want live terminal output use `ExecRequest::debug = true`, which tees the captured bytes through a redaction-aware drain rather than skipping capture.
- **Stdin mode.** `StdinMode::Inherit` (default), `Null`, or `Bytes(Vec<u8>)`. `Bytes` allocates a stdin pipe and a writer thread; the other modes do not.
- **Environment mode.** `EnvironmentMode::Inherit` (default) or `ClearAndSet(pairs)`. `ClearAndSet` calls `command.env_clear()` then sets the supplied pairs. The `Debug` impl redacts values for keys that match `is_sensitive_env_name`.
- **Process group leadership (Unix).** Children spawn with `command.process_group(0)`, so the child's PGID equals its PID. Non-Unix builds skip this step.
- **Working directory.** `current_dir` is applied to the spawn before the child runs.
- **Spawn failure.** If the OS fails to spawn the program, `run_process` returns `OrbitError::Execution("failed to spawn `<program>`: <error>")` and never enters the supervision loop.

## Supervision Invariants

- **Background drains.** `wait_with_optional_timeout` spawns reader threads for stdout and stderr immediately after spawn. The child must never block on a full pipe buffer because the parent is not reading.
- **Stdin writer thread.** When `StdinMode::Bytes` is set, a writer thread copies the payload to the child's stdin. A failed write terminates the child via `terminate_process_group` and surfaces as `OrbitError::Execution(<message>)`.
- **Poll interval.** The wait loop polls with `WAIT_POLL_INTERVAL = 100ms` (or the remaining deadline, whichever is smaller). The interval is global and not per-request configurable.
- **Signal handler installation (Unix).** A `SignalHandlerGuard` refcounts process-wide SIGINT and SIGTERM handlers for the duration of the wait loop. The first live waiter installs the handlers and snapshots the previous `sigaction` structs; the last drop restores them and re-raises a captured signal so the previous disposition still runs (tokio shutdown, or SIG_DFL terminate). `SIG_IGN` is not re-raised. SIG_DFL additionally writes `process interrupted by signal SIG…` to the process stderr before `raise`. The install mutex is not held across the wait or across `raise`, so concurrent supervised waits overlap. Each waiter registers its child's pgid in a lock-free table; the handler `killpg`s every registered group, records a pending forward, and bumps a generation counter that waiters poll.
- **Timeout escalation.** When the deadline expires, `terminate_process_group(child, SIGTERM, poll_interval)` is called. If the group does not exit within `TERMINATION_GRACE_PERIOD = 5 seconds`, `kill_process_group` (SIGKILL) is invoked plus a direct `child.kill()`/`child.wait()`.
- **Parent-signal escalation.** When the parent receives SIGINT or SIGTERM during the wait, the same termination path runs with the received signal. The result reports `exit_code = Some(128 + signal)` and `success = false`.
- **Clean-exit reaping.** When the child exits cleanly, the wait loop calls `kill_process_group(child.id())` to reap any orphan subprocesses still holding pipe write ends, then joins the reader threads. Without this, an orphan grandchild can keep the pipes open and block reader-thread completion indefinitely.
- **Stderr annotation.** Timeouts append `process timed out` to stderr; parent-signal interruption appends `process interrupted by signal SIG<NAME>`. The annotations are added before the result is constructed, not by the caller.
- **Exit code reporting.** `ExecutionResult::exit_code` is `Some(code)` for clean exits, `Some(128 + signal)` for parent-signal exits, and `None` for timeouts.

## Result Shape

`ExecutionResult { success, stdout, stderr, exit_code, duration_ms, output }`:

- `success` reflects the child's exit status (clean exit with zero status). Timeouts and parent-signal exits report `success = false`.
- `stdout` and `stderr` are `String::from_utf8_lossy` conversions of the captured bytes. Non-UTF-8 output is preserved as replacement characters.
- `duration_ms` is wall-clock time from `Instant::now()` at spawn entry to spawn return.
- `output` is reserved for callers that want to attach a parsed-output payload after the fact; `run_process` itself does not populate it.

## Failure Modes

- **Spawn failure.** `OrbitError::Execution("failed to spawn …")` — caller cannot retry without changing the request.
- **Stdin write failure.** Writer-thread error → child terminated → `OrbitError::Execution(<error>)` returned. Captured stdout/stderr up to that point are discarded.
- **Stdin writer panic.** Writer-thread panic → `OrbitError::Execution("stdin writer thread panicked")` returned.
- **Signal handler install failure.** If `sigaction` fails for SIGINT or SIGTERM, the guard rolls back any partial install and `run_process` returns `OrbitError::Execution(<error>)` before entering the wait loop.
- **Wait error.** `child.wait_timeout` errors surface as `OrbitError::Execution("wait timeout error: …")`. The child is left to be reaped by the OS rather than force-killed in this path; this is a known soft spot.
- **Timeout.** `success = false`, `exit_code = None`, stderr suffixed with `process timed out`.
- **Parent signal.** `success = false`, `exit_code = Some(128 + signal)`, stderr suffixed with the signal name.

## Concurrency Constraints

- **Shared signal-handler install, overlapping waits.** On Unix, concurrent `run_process` / `supervise_child` calls share one SIGINT/SIGTERM disposition (refcounted). Their wait loops overlap. Ctrl-C terminates every live registered process group; waiters that could not claim a pgid slot still observe the generation counter and run the ordinary termination path. After the last waiter restores the previous disposition, the captured signal is re-raised so the parent still shuts down.
- **No assumption about thread-local state.** Reader threads, writer threads, and the signal handler are spawned with `'static` requirements; callers must not rely on thread-local data from the spawning thread.
- **No retry inside `run_process`.** The runner does not retry spawn failures, wait errors, or signal-install failures. Retry policy belongs to the caller.

## Migration Rules

- New `ExecRequest` fields must default to a backwards-compatible behavior; `EnvironmentMode::default()` and `StdinMode::default()` exist precisely so callers can adopt new fields incrementally.
- The current `Sandbox` trait only exposes request validation. Adding live confinement requires an explicit confined-spawn seam or platform wrapper before untrusted code runs; returning successfully from `validate` alone cannot establish that boundary.
- Changes to `TERMINATION_GRACE_PERIOD` or `WAIT_POLL_INTERVAL` require updated current documentation and behavior tests because both constants are observable in timeout/cancel behavior.

## Agent Signature

Live-read investigation and spawn-seam clarification revised by codex on 2026-09-07.

## Live read enforcement investigation (2026-09-07)

**Design and isolated prototype only. No production read boundary was added.**
The activity-scoped `proc.spawn` implementation still checks apparent path
arguments before ordinary spawn. An admitted git shell alias can read outside
that argument check. The preserved candidate `2c40c430` is not a complete repair
and must not be landed as one. The authoritative original task evidence remains
in ORB-11514's `read-boundary-probe.py`, `read-boundary-probe.json`, and its
`enforcement_design_blocked` execution summary. This investigation's command
outputs and grant manifests are attached to ORB-11546.

### Mechanism selection and limits

| Mechanism | Live names / generated files | Availability and maintenance | Decision |
| --- | --- | --- | --- |
| Preserved candidate Landlock path-beneath rules | Directory grant admits future denied names; file grants break new allowed files and follow renamed inodes | Unprivileged on supporting Linux kernels; small existing syscall seam | Useful additional host containment, insufficient for `denyRead` |
| Existing Bubblewrap mounts and post-run guard | Masks existing paths; cannot deny every future matching basename in a writable tree; after-exit checks cannot recover leaked bytes | Requires admitted namespace setup; already owned by orbit-exec | Retain write isolation; not the missing live read layer |
| AppArmor pathname LSM, stacked before exec | Candidate for checking actual resolved opens and new names; allowed creation remains possible | Requires enabled LSM, operator-loaded enforcing profile, allowed stacking, parser/version support | Smallest next Linux experiment; offline compilation works, live profile unavailable in this runner |
| Seccomp notification with pathname check then `CONTINUE`; ptrace pathname filter; preload wrapper | Pointer mutation or path rename can invalidate userspace decisions; preload also misses direct syscalls/static binaries | A syscall tracer needs architecture and descendant coverage | Reject as a security boundary in this form |
| Seccomp broker that performs opens and injects FDs | Can avoid tracee-pointer races by copying arguments and using `ADDFD`; still must bind policy to the actual object and cover mutation, alternate I/O and descriptor acquisition | New broker lifecycle, syscall/ABI compatibility, deadlock and resource budgets | Acquisition alternative; FD injection alone does not revoke later access |
| Filtered filesystem / FUSE | Could own name mutations and use the canonical evaluator; raw backing access must be inaccessible | Requires mount admission and a filesystem service; caching, mmap, hardlink and external-writer semantics need design | Alternative for owned mutations; caching and previously acquired bytes remain separate constraints |

Landlock governs filesystem objects and hierarchies, not a negative basename
language. Its documented ABI rules also make cross-directory rename/link fail
without `REFER`; a nominally read-only ruleset is not transparent to builds.
The prototype explicitly handles ABI 2 `REFER` and grants it only within the
synthetic workspace. This retains host isolation while permitting cargo's output
moves. [Linux Landlock contract](https://docs.kernel.org/userspace-api/landlock.html).

AppArmor supplies pathname rules, deny precedence, execution inheritance (`ix`)
and hardlink permission-subset checks. The prototype emits a fixed fixture
policy, denies both read and executable mapping/execution for `.env`/`*.env`,
and uses **`aa_stack_onexec`**, never a replacing profile transition. Stacking
intersects the existing confinement. It does not authorize policy loading or
relax the enclosing provider sandbox.
[AppArmor profile syntax](https://manpages.ubuntu.com/manpages/noble/man5/apparmor.d.5.html),
[AppArmor stacking API](https://apparmor.net/man/master/aa_stack_profile/).

The seccomp alternative would need to perform the authorized open itself and
inject its FD, not resume a syscall using a mutable pathname pointer. Even then,
checking a pathname and later opening it leaves filesystem races. Safe resolution,
mutation ownership, notification cancellation, and non-open interfaces remain
part of that design. [Kernel seccomp notification contract](https://docs.kernel.org/userspace-api/seccomp_filter.html).
FUSE does not automatically make these decisions correct: its I/O modes include
kernel caching and mmap behavior that a policy filesystem must account for.
[Kernel FUSE I/O contract](https://www.kernel.org/doc/html/latest/filesystems/fuse/fuse-io.html).

### Reproducible isolated probe

`crates/orbit-exec/tests/live_read_probe.py` extends the original synthetic probe
approach. It is an explicitly invoked stdlib Python evidence collector, outside
Cargo's normal test discovery. It does not implement Orbit policy evaluation or
change sandbox defaults. `prepare` refuses an existing directory and writes only
its new fixture. All attempted forbidden contents are synthetic. `run` prints
exact argv, child outputs, outcomes, elapsed milliseconds and the declared grants.
Exit 1 means at least one probe failed; exit 2 means unavailable probes without
other failures. Neither a zero collector exit nor offline profile compilation
certifies the full production contract.

```sh
python3 crates/orbit-exec/tests/live_read_probe.py prepare /tmp/orbit-live-read-UNIQUE
apparmor_parser --skip-kernel-load --skip-cache /tmp/orbit-live-read-UNIQUE/profile.apparmor
python3 crates/orbit-exec/tests/live_read_probe.py run /tmp/orbit-live-read-UNIQUE --backend baseline
python3 crates/orbit-exec/tests/live_read_probe.py run /tmp/orbit-live-read-UNIQUE --backend landlock
python3 crates/orbit-exec/tests/live_read_probe.py run /tmp/orbit-live-read-UNIQUE --backend apparmor
```

`baseline` retains outer confinement, adding no new sandbox. The paired baseline
must return the synthetic forbidden data so a missing executable, failed setup,
or empty output cannot masquerade as enforcement. Negative probes require an
executed worker reporting `EACCES`/`EPERM`; failure to enter the worker is
**unavailable**, not pass. Creation/rename setup failures are also unavailable.
Generated-file positives require the exact allowed content. git aliases invoke a
real child interpreter, and the descendant probe forks and calls `setsid` before
attempting the outside read. The separate rerun of the original artifact preserves
the exact `!cat <outside-sentinel>` regression and direct `/etc` CLI control.

The profile is intentionally a fixed `.env`/`*.env` experiment. Its path renderer
rejects metacharacters rather than guessing AppArmor escaping. It neither accepts
arbitrary Orbit glob policies nor asserts semantic parity with them. It permits
writes to new denied-name fixtures to test read denial independently; production
modify authority must still come from the effective modify profile.

### Narrow host grants and recovery

The manifest separates workspace access from each host grant and explains its
purpose. There is no blanket `/`, `/etc`, home, `/proc`, `/dev`, Cargo home, or
provider credential-tree grant. Canonical paths are recorded, so resolver
symlinks do not require granting their entire target directory.

- Exact resolved git, rg, shell, Python, make, gh, compiler and rustup executable
  paths; distribution library directories and git helper/template directories.
  These are runtime dependencies, not permission for arbitrary host user files.
- Exact loader cache, `/dev/null`, `/dev/urandom`, NSS/hosts/resolver/gai files and
  the CA bundle. `GIT_SSL_CAINFO` selects that bundle explicitly instead of
  granting the complete certificate directory. Network rules permit IPv4/IPv6
  TCP and UDP; endpoint authorization is a separate network-policy concern.
- Exact rustup `settings.toml`, directory-list permission on `toolchains` (no
  inherited subtree access), and the selected installed toolchain's `bin` and
  `lib` trees. Cargo's home, build output, temporary files and git/gh configuration
  are fixture-owned. The child environment is rebuilt from declared values.
- No GitHub, SSH, cloud or model-provider credential is read. `gh --version`
  is a startup check; `gh api meta` without credentials reports its authentication
  requirement. That failure is preserved, not counted as network recovery success.
  Authenticated recovery still needs an explicitly authorized GitHub credential
  capability and a controlled integration test. It cannot inherit unrelated
  provider write grants as read authority.

Measured on Linux x86_64, kernel `6.8.0-139-generic`, Landlock ABI 4: outside,
symlink-outside and detached-descendant reads are denied under the host grants;
existing denied names, dynamic denied creation and rename, and hardlink aliases
still leak under the deliberately insufficient directory Landlock backend.
Generated files, git, rg, make, offline cargo check and TLS git are exercised in
the attached final run. These positives only demonstrate runtime grant viability
under Landlock, **not** AppArmor or production `proc.spawn` compatibility.

The first recovery run failed opening rustup's toolchain directory and Git's
certificate directory. File-only directory-list permission and explicit CA-bundle
selection resolved those failures. Cargo then reached compilation but failed
moving its `.rmeta` with `EXDEV`; explicit workspace `REFER` addresses that
independent kernel restriction. Initial failures and a file-syscall trace are
retained as evidence rather than discarded.

### Races, aliases and the contract decision

One candidate contract is **authorization at acquisition of new file access**,
using the resolved kernel path, with controlled descriptor inheritance.
It is not retroactive erasure of data previously read. This interpretation needs
an explicit owner decision before production implementation; this task does not
silently amend the original mandate.

| Case | Required handling / remaining evidence |
| --- | --- |
| New denied name or allowed inode renamed to a denied name, then reopened | Kernel pathname check must deny the new open before bytes enter the child; original regression retained. AppArmor live result unavailable here. |
| Concurrent symlink/rename swaps | No userspace check-then-open boundary. Kernel LSM is the candidate acquisition boundary; require concurrent adversarial open/openat/openat2 and parent-directory rename probes on the actual host. Sequential success cannot prove race freedom. |
| Symlinks and alternate workspace mounts | Match resolved target as `PolicyEngine::check_resolved` does. Validate both canonical checkout and `/tmp/orbit-workspace` mount views, dangling links and `/proc` magic links. Never add a broad proc grant to accommodate one runtime file. |
| Hardlinks | Names do not track secret provenance. AppArmor's subset rule can reject a child creating a more permissive alias, but an already existing allowed-name hardlink is a different case. Define resolved-path semantics consistently with the current evaluator, or require an isolated backing tree with no external hardlink writers. Neither is byte-provenance tracking. |
| File opened while allowed, then renamed; existing mmap | The Landlock probes retain access. Linux 6.8 AppArmor caches granted FD permissions and does not provide general rename-based revocation. A mapping can already expose bytes without another pathname syscall. If revocation after rename is required, neither candidate is a full solution; specify stronger mutation isolation or a narrower acquisition contract explicitly. |
| Inherited FD, cwd/dirfd, SCM_RIGHTS and pidfd acquisition | Close all nonessential descriptors before untrusted exec; use owned stdin pipes/null rather than arbitrary inherited files. Restrict Unix-socket FD donors, ptrace/process-memory and other access channels. AppArmor transition revalidation is not a substitute for descriptor hygiene. The deliberate inherited-FD probe demonstrates the Landlock hole. |
| Descendants and detached sessions | Enforce before the first untrusted instruction; use inherited/stacked confinement across fork/exec. `setsid` does not remove Landlock or AppArmor. Termination of processes escaping the original PGID is a separate supervision problem; the probe's detached child exits and is waited for explicitly. |
| External writers or already known bytes | A denied name cannot undo copies, prior reads or malicious externally supplied hardlinks. Record the trusted-writer/acquisition boundary; do not advertise retroactive secrecy. |

The descriptor limit above is supported by the Linux 6.8 implementation's cached
permission path and lack of general revocation, not by an assumed property of a
profile regex. [AppArmor file permission implementation](https://raw.githubusercontent.com/torvalds/linux/v6.8/security/apparmor/file.c).

Linux runtime directories and SQLite sidecars are an object-authority exception
to path-only compilation. The host must open each accepted object while it is
validating or descriptor-relatively creating it, carry that descriptor through
engine dispatch, and supply `--bind-fd` as Bubblewrap's bind source. The
pathname remains the namespace destination only. Replacing a validated name
with a symlink or different object must either leave the held object as the sole
writable source or reject the plan before spawn. A second canonicalization or
metadata check without a retained descriptor does not meet this contract.

The guarantee covers renames and link replacement beneath the opened runtime
root. It assumes an unprivileged peer cannot remount the runtime root or its
host ancestors. Bubblewrap consumes and closes the inherited setup descriptors
before provider exec; the parent closes its copies with the plan after spawn.
Non-Linux backends do not consume this authority representation.

### Ownership and eventual implementation targets

Keep one semantic evaluator. `orbit-types/src/policy/policy_def.rs` owns ordered
rule semantics and `policy/glob.rs` owns the grammar; `orbit-policy` owns resolved
filesystem decisions. A platform compiler must consume that canonical policy and
prove equivalence with `PolicyDef::check_path`, including zero-segment `**/`,
literal metacharacters, normalization, profile negations and final global denies.
AppArmor's deny precedence is not equivalent to arbitrary last-match re-allows.
Reject unrepresentable policies explicitly until a complete compilation exists;
that rejection is a compatibility blocker, not completed implementation.

Concrete follow-on targets, **not modified here**:

1. `orbit-types` policy contracts plus the owning Core activity-grant composition:
   separate immutable, purpose-attributed host read/execute dependencies from
   workspace rules and provider write grants. Unknown/absent grants fail closed.
2. `orbit-policy` and shared Types grammar: canonical compilation semantics and
   differential tests. `orbit-exec` currently depends on Types/Common, not Policy;
   do not add the backwards edge casually. Pass a shared compiled contract or
   review an explicit architecture update before adding a dependency.
3. `orbit-exec/src/sandbox.rs`, `process.rs`, `runner.rs`: add an authoritative
   confined-spawn seam. The current trait only validates; a wrapper must not spawn
   in `validate` and then also take the ordinary unconfined spawn path. Keep
   existing supervision and outer containment; verify attachment before exec.
4. `orbit-tools/src/builtin/proc/spawn.rs` and `tests/proc_spawn_lockdown.rs`:
   supply the effective activity authority and use that seam; retain explicit
   path preflight as a fast diagnostic, exact allowlist, and cleared environment.
5. Existing platform sandbox modules and Core admission: capability/attachment
   checks, bounded profile lifecycle, unsupported-platform failure before exec,
   cleanup and diagnostics. No retry through `NoSandbox` on failure.

### Acceptance mapping and operator handoff

Each original ORB-11514 criterion remains required for the production repair.

| Original criterion | Enforcement point and present evidence / unresolved gate |
| --- | --- |
| 1. Indirect host and denied-name containment | Confined spawn plus kernel path checks; original alias still reproduces the open production bug. Landlock host denial works, live name enforcement is not proved. |
| 2. Sentinel alias and direct `/etc` regression | Original artifact rerun retains both controls; new worker requires actual denied opens. Must port both into scoped `proc.spawn` integration tests after repair. |
| 3. git/rg, program allowlist and cleared environment | Narrow-grant tool positives plus existing `proc_spawn_lockdown` boundary tests; fixed prototype environment is not a replacement for the production allowlist tests. |
| 4. Authoritative boundary, focused tests and CI | Runtime seam specified above; no production boundary change. This leaf runs `make ci-fast` and `make ci-lint`, but they cannot certify missing enforcement. |
| 5. Runtime/config/resolver needs under declared grants | Manifest and actual git/rg/make/cargo/TLS-git results; authenticated gh, package downloads, credential-helper workflows and real recovery activity boundary remain integration gates. |
| 6. Dynamic names, unsupported platforms and bounded cost | Landlock failures preserved; AppArmor live probes unavailable. Linux-only collector explicitly rejects unsupported Landlock targets. Production macOS/Windows behavior is unchanged and must fail closed in the repair until separately validated. |

On this runner AppArmor is enabled and the outer label is
`bwrap//&unpriv_bwrap (enforce)`. Offline parsing succeeds, but
`aa_stack_onexec(<fixture-profile>)` returns `ENOENT`: the generated profile has
not been loaded into the visible policy namespace. No policy load was attempted.
All AppArmor child probes are therefore **unavailable**. The original nested
Bubblewrap probe still reports namespace creation denied. This says nothing
about whether the operator's host-side launcher can create namespaces.

The next operator-side experiment is concrete:

1. On the intended host, prepare a unique fixture using the commands above,
   inspect its manifest/profile, and compile it with `--skip-kernel-load`.
   The profile contains no host write grant except `/dev/null`.
2. With separately authorized administration, load only that named experimental
   profile using `apparmor_parser --add --skip-cache <fixture>/profile.apparmor`.
   Confirm its enforcing mode and that the existing parent label is retained
   when stacking. The executor must not self-authorize this administration.
3. Run the AppArmor backend from the actual admitted execution context. Require
   executed negative and positive bodies, no unavailable rows, the exact original
   alias sentinel, and the resolved-policy differential/race tests above. An
   ENOENT/EPERM result is an admission blocker, never permission to leave the
   provider sandbox or switch to an unconfined backend.
4. Separately run `/usr/bin/bwrap --die-with-parent --unshare-user --unshare-pid
   --ro-bind / / --proc /proc -- /usr/bin/true` as the intended host execution
   identity and through normal activity admission. Record both argv, exit status,
   stderr, uid/kernel and the current AppArmor label. This tests namespace
   admission only; it is not a read-confidentiality test.
5. After every experimental process exits, an authorized operator removes only
   this profile with `apparmor_parser --remove <fixture>/profile.apparmor` and
   deletes only the corresponding run-owned fixture. Preserve the JSON evidence.

Performance has two different gates. The prototype emits a bounded number of
rules from explicit grants, without recursively walking the workspace; the
Landlock setup performs one open/add per grant. Five offline AppArmor parser runs for the 1,885-byte fixture profile took
17.098–23.656 ms; per-command wall times are also attached, including
failed/unavailable runs.
There is **no measured live AppArmor overhead**. Before selection, measure cold
profile compilation/load and warm exec, 3,000-file and large-tree git/rg workloads,
concurrent jobs, parser memory, and policy size limits; compare distributions with
the admitted baseline. Cache by policy/grant/platform identity, not mutable file
inventory. Bound compilation time, profile count and cleanup; do not extrapolate
from a single small fixture or count failure latency as successful throughput.

The pending decisions are therefore: authorize and provision the host-side LSM
experiment; ratify acquisition-time versus revocation/provenance semantics; define
an explicit GitHub credential capability for authenticated recovery; and require
separate supported-platform evidence. Until those gates settle, this is a
reviewable experiment and design handoff, not proof of a complete production fix.

### Revised evaluation and executable handoff (2026-09-08)

The earlier measurements above remain historical evidence. The original installed
`proc.spawn` artifact was rerun unchanged: the `!cat` alias still returns
`SCOPED_HOST_SENTINEL`, direct `git -C /etc` returns `policy_denied`, directory
Landlock grants admit dynamic denied files, and file-only grants both block new
allowed files and retain access to renamed inodes. The revised isolated
fixture adds deterministic classification tests, paired baseline controls, a
preexisting hardlink, and a read-before-rename memory control. It also exercises
real git, cargo and gh clients against a local authenticated fixture. It still
makes **no whole-contract feasibility claim** and changes no production defaults.

Run the complete experiment from a new directory:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s crates/orbit-exec/tests -p test_live_read_probe.py -v
PYTHONDONTWRITEBYTECODE=1 python3 crates/orbit-exec/tests/live_read_fixture.py prepare /tmp/orbit-live-read-UNIQUE --execution-context admitted-worker > /tmp/orbit-live-read-prepare.json
PYTHONDONTWRITEBYTECODE=1 python3 crates/orbit-exec/tests/live_read_fixture.py run /tmp/orbit-live-read-UNIQUE --execution-context admitted-worker > /tmp/orbit-live-read-evidence.json
```

`prepare` prints the exact compile, load, test and remove commands for its unique
profile. It never executes load/remove. An authorized operator owns profile
provisioning and removal after all experimental children exit. Use
`--execution-context operator-host` on the actual host; this is an explicit
operator assertion, not an automatic attestation. Run through normal worker
admission as a separate measurement. Profile attachment is still stack-only and
cannot relax the outer label. Preserve the JSON and generated profile before
removing the run-owned temporary fixture; do not remove any shared profile or
workspace. A failed load must not be followed by a replacement/unconfined exec.

The harness records the exact minimal bwrap command, uid, kernel, parent label,
and `/dev/fuse` presence separately from confinement results. In this worker the
minimal namespace probe fails with “No permissions to create new namespace” and
`/dev/fuse` is absent. The orchestrator's task comment records an exit-0 host
namespace probe on the same date. These are different execution contexts; neither
result establishes an enforcing AppArmor profile or a usable FUSE mount. No host
policy load or namespace bypass was attempted by this leaf.

The local recovery server binds an ephemeral IPv4 loopback port in a separate
process and exits in a `finally` cleanup after the clients finish. It serves a
small Git repository containing a library dependency and a fixture JSON endpoint.
Every request requires the synthetic bearer token. The manifest grants exactly
`<fixture>/recovery-token.txt`, a mode-0600 file outside the workspace, in addition
to the existing explicit runtime grants. No real token or ambient credential is
used. Client environments are cleared and composed from the manifest. The shell
reads that exact file; git receives an explicit HTTP header, cargo invokes git to
fetch the dependency, and gh receives the fixture header/token. Each run uses a
fresh clone, Cargo home and build directory, so a warm cache cannot stand in for
a successful dependency fetch. Request evidence records only path and whether
authentication matched; each backend gets only its own request log slice.

`--local-recovery` on the lower-level probe replaces the two public-network probes
with these three local checks. Without it, the original TLS-git and credential-free
gh probes remain available. Local HTTP proves capability plumbing and cold fetch,
not TLS/resolver behavior, a real provider credential helper, or actual production
activity admission. The earlier TLS result remains historical, not a current
local-fixture pass. The fixture server is trusted infrastructure outside the
client restriction, not a filesystem broker or part of the enforcement claim.

Current paired results on Linux `6.8.0-139-generic`:

| Boundary or behavior | Baseline | Landlock | AppArmor |
| --- | --- | --- | --- |
| Outside, symlink-outside and detached descendant open | Synthetic bytes returned | EACCES; no marker returned | Unavailable: stack-on-exec ENOENT |
| Existing denied name, dynamic creation and rename then reopen | Synthetic bytes returned | Synthetic bytes returned | Unavailable |
| Child-created and preexisting allowed-name hardlinks | Synthetic bytes returned | Synthetic bytes returned | Unavailable |
| Inherited outside FD | Synthetic bytes returned | Synthetic bytes returned | Unavailable |
| FD read and mmap access after rename to denied name | Synthetic bytes returned | Synthetic bytes returned | Unavailable |
| Bytes read into memory before rename, output after rename | Synthetic bytes returned | Synthetic bytes returned | Unavailable |
| New allowed files, git/rg/make and offline cargo | Pass | Pass | Unavailable |
| Authenticated local git clone, cold cargo dependency build, gh API | Pass; 13 authenticated requests | Pass; 13 authenticated requests | Unavailable; zero requests |

The 22-row local suite reports baseline 10 pass / 8 fail / 4 observations,
Landlock 13 pass / 5 fail / 4 observations, and AppArmor 22 unavailable / zero
passes. Raw observation rows are preserved for compatibility; the new
`contract_assessment` independently marks descriptor acquisition, later-access
revocation and previously acquired byte secrecy **failed**, not satisfied or
ignored. Missing rows are unavailable. Paired negative controls require a
successful baseline body returning the marker before a candidate denial counts
as exercised. A marker returned before a crash, timeout or malformed response is
a confidentiality failure. Empty output, malformed JSON, descendant failure,
setup failure and ENOENT are never permission-denial passes. Eleven deterministic
tests cover these rules; focused tests against the original classifier reproduce
its malformed-output and leak-after-failure defects.

### Exact remaining contract decisions and alternatives

No candidate currently meets all the requested semantics. These are separate
requirements, not one profile-provisioning problem:

1. **New pathname acquisition:** retain the requirement that resolved forbidden
   opens cannot return bytes, including create/rename and racing link changes.
   Landlock demonstrably fails dynamic names. AppArmor remains the smallest
   unproved pathname candidate. Its profile must be loaded and tested through
   normal admission, with adversarial open/openat/openat2, concurrent symlink and
   parent-directory swaps, and canonical/stable mount views. This harness's
   sequential cases do not claim race coverage or arbitrary-policy equivalence.
2. **Alias provenance:** the new preexisting hardlink probe has one inode with
   both an allowed and a denied name before confinement. Denying creation of a
   new alias cannot fix that case. The current resolved-path evaluator checks
   the selected name, not every alias. An owner must choose resolved-name
   semantics or require a privately owned backing tree with explicit alias
   rejection/provenance rules and no external writers. This task adopts neither
   choice as a silent contract amendment.
3. **Later FD/mmap access:** retaining ordinary readable descriptors while
   requiring later pathname changes to revoke them needs a different mechanism
   from acquisition-only LSM/broker opens. The FD and mmap probes retain this
   failure as an explicit gate. A policy filesystem could mediate uncached reads
   while serializing its own mutations, but cached pages and mappings require a
   separately validated design. Disabling mmap would also change tool behavior;
   passing `rg --no-mmap` alone would not prove general rg compatibility.
4. **Previously acquired bytes:** `rename_cached` reads an allowed file into
   ordinary process memory, renames it, then outputs only the saved memory.
   A pathname filesystem, AppArmor rule, or open broker has no later file read to
   reject. Requiring erasure of those bytes while allowing the original read and
   rename is incompatible with this boundary. An owner must explicitly exclude
   retroactive memory secrecy, prohibit the transition, or require an entirely
   different information-flow execution model. Output marker filtering is not a
   repair because the child already possessed the bytes and can encode them.
5. **Descriptor admission:** close nonessential inherited FDs and use owned stdin;
   account separately for dirfds, SCM_RIGHTS, pidfd and process-memory interfaces.
   The inherited-FD result proves that pathname grants alone do not suffice.
   A production fix cannot simply omit that row from acceptance.

A seccomp open broker using copied arguments and FD injection remains a possible
**acquisition** implementation after these decisions. It must never use pathname
validation followed by `CONTINUE`, and must own safe object resolution and all
alternate descriptor acquisition routes. Returning a readable FD does not repair
items 3–4. A FUSE prototype likewise cannot establish item 4: cached userspace
bytes bypass filesystem requests regardless of its I/O mode. Thus adding a new
broker here would not resolve the measured whole-contract incompatibility, and
none is represented as a proven substitute. Kernel documentation describes
[notification/FD injection](https://docs.kernel.org/userspace-api/seccomp_filter.html)
and [FUSE caching and mmap modes](https://www.kernel.org/doc/html/latest/filesystems/fuse/fuse-io.html);
the memory-control conclusion follows from the executed probe, not a claim that
these facilities provide revocation.

Keep the canonical evaluator and production targets in the ownership section
above. The next implementation must supply explicit semantics for items 1–5,
compile the full ordered policy without duplicating it in the transport, and use
one confined spawn path. Gate host-grant composition and credential admission in
Core, resolved policy semantics in Types/Policy, and attachment/lifecycle in Exec.
Require original sentinel/direct-path controls plus program/environment fail-closed
tests at Tools, and real kernel denial/positive tests at Exec. Unsupported
platforms must reject before untrusted execution. No production default switch,
release, or landing of the unsafe candidate is authorized by these probe results.

The original six-criterion mapping above remains current: criteria 1/2/6 retain
live name/race/availability gates; criterion 3 has additional real client evidence
but still needs production allowlist/environment regression tests; criterion 4
retains the authoritative spawn integration gate; criterion 5 now has cold local
authenticated recovery evidence, with actual activity admission and real-service
validation explicitly outstanding. Neither local tests nor workspace lint turn
those unmet production criteria into passes. The collector records per-command
latency, including failures; no live AppArmor overhead, large-tree bound or
supported-platform performance is claimed from these measurements.
