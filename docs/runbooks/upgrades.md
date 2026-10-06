---
type: runbook
summary: Install a new Orbit release with `orbit update`, then review, apply, and verify workspace-layout and store-schema migrations safely, including what an older binary may still do with a newer workspace.
tags: [operations, upgrades, migrations, recovery]
paths: ["crates/orbit-cmd/src/update/**", "crates/orbit-common/src/fs/generation/**", "crates/orbit-store/src/workflow/layout/**", "crates/orbit-store/src/driver/sqlite/migration/**", "crates/orbit-store/src/contracts/compat.rs"]
related_features: [orbit-core]
related_artifacts: [ORB-10014, ORB-11280, ORB-11344, ORB-11695, ORB-11753, ORB-12013, ORB-12434, ORB-13631]
last_validated: 2026-10-05
---

# Upgrade Orbit Safely

Use this runbook to replace an Orbit binary that may introduce workspace-layout or
store-schema migrations.

## Upgrade with `orbit update`

```sh
orbit update --check              # is a newer release published? exits 3 when yes
orbit update                      # install the newest published release
orbit update --version 0.19.0     # install one exact release
orbit update --allow-downgrade --version 0.18.0
orbit update --json               # machine-readable report
```

`--check` exits `0` with `outcome: already_current` when the resolved release is
equal to or older than the running version. This includes a prerelease newer than
the latest stable release, or a mirror whose published version lags the installed
one. Only a newer target reports `outcome: update_available` and exits `3`.
The check downloads no release archive and does not converge workspace state.

`orbit update` does the whole upgrade in one defined order:

1. Resolve the target version — the newest published release, or the one `--version` names.
2. Refuse an installation Orbit's own installer does not own. An npm, Homebrew, `cargo
   install`, or checkout build is reported with the command that *does* upgrade it, before
   anything is downloaded. A Homebrew install names the fully qualified canonical formula,
   `constellation-works/tap/orbit`, rather than an ambiguous `brew upgrade orbit`. A machine
   that still has the retired `danieljhkim/tap/orbit` formula installed gets a tested migration
   sequence instead — uninstall the legacy formula, then install the canonical one — because the
   two conflict rather than coexisting; a canonical-only install gets the ordinary qualified
   upgrade.
3. Acquire generation admission against the same resolved authorities `--preflight` uses,
   refusing while any participating Orbit process is live, then take the exclusive
   install-directory lock so two updates cannot interleave.
4. Re-read the installed binary's version under that lock, and on Linux resolve a replaced
   running inode (`/path/to/orbit (deleted)`) back to the live install path. Equal, newer,
   and older installed versions are decided from that evidence — a writer that started on
   an older snapshot cannot overwrite a newer install that finished while it was discovering
   a release. `--check` stays read-only and does not take the lock.
5. Download the release archive, authenticate the checksum manifest against the trusted
   release signing keys, compare the archive's SHA-256, and extract its single `orbit` member
   into a staging file beside the installed one. Run the staged executable's `--version`
   and require it to match the requested release before copying a backup or replacing
   anything. A mislabeled release, failed version probe, or unparseable version is refused
   with the installed executable untouched.
6. Copy the current executable to `<orbit>.previous`, then swap the staged file in with one
   atomic same-directory rename, and confirm the installed binary reports the requested
   version. If it does not, the previous executable is copied into a complete sibling staging
   file and atomically renamed over the replacement, so concurrent launches see either the
   complete replacement or the complete previous executable; the retained backup is not consumed
   and no workspace state is touched.
7. Run `orbit migrate --confirm`, then `orbit workspace sync` — **using the newly installed
   binary**, in the selected workspace. Orbit passes the resolved root to both subprocesses;
   an explicit `--root` remains authoritative even when `ORBIT_ROOT` names another workspace.
   Only the new binary carries the migrations and managed asset definitions for the version
   being installed.
8. Run `orbit clock repair`, again as the newly installed binary. The launchd/systemd sweep
   unit embeds an absolute program path, so an install that lands somewhere else — Homebrew
   to `~/.orbit/bin`, say — leaves the unit invoking a binary that may no longer exist. The
   step rewrites the unit to the installed binary and re-registers it, and the update report
   carries the line it printed. A unit that already names this binary is left untouched; a
   paused clock is corrected on disk but not resumed.

Migration runs before managed-asset sync because a layout migration can move the directories
those assets live in. The clock unit is converged last, and only after the workspace steps
succeed: a clock re-armed against a half-migrated workspace would just fail every minute.
Unlike the first two, it is host state, so it runs even outside an initialized workspace.

`install.sh` runs the same `orbit clock repair` after it installs the binary, so a host that
moves between install locations (Homebrew or `cargo install` to `~/.orbit/bin`) does not need
`orbit update` to notice. A binary installed by a package manager that runs neither — `brew
upgrade`, `cargo install` — still needs one `orbit clock repair` by hand; `orbit doctor`'s
`clock-unit` row and a hand-run `orbit sweep` both name it.

Everything before the swap fails with nothing changed. After the swap the command never
reports success on an incomplete upgrade: it exits `4` with `outcome: needs_recovery` and
names the step that failed.

### Upgrade admission: compatibility generations

Every participating Orbit process — a one-shot command, `orbit mcp serve` (stdio,
the TCP listener, the local part of a federated mux, destination-side SSH servers),
the dashboard, a clock tick, and every pipeline worker — joins its **generation
authority** before runtime bootstrap and holds that membership until it exits.
Admission follows `compatibility-generation-v2`: it asks whether the binaries'
*state versions* are compatible, not whether their executables are identical.

Each binary is compiled with a **compatibility identity**, which `orbit update
--contract` reports:

- the store schema and the workspace layout ledgers, each as the newest version the
  binary migrates to plus two floors — the newest migration an older binary cannot
  keep **writing** through, and the newest one it cannot **read** (see
  [Run an older binary against a newer workspace](#run-an-older-binary-against-a-newer-workspace)
  for how migrations are classified);
- every feature schema (automation, review, local pull) at the version the binary
  migrates it to. Feature ledgers refuse any newer version, so they must match.

The authority records the envelope of every identity admitted since it last had no
participant in `.generation-compat.json`, and each live process registers a
record — pid, role, access, digest, identity and start time — under
`.generation-participants/`. The next joiner replaces that envelope when it can
take `.generation.lock` exclusively, which happens only after every previous
holder has exited. An exited process does not stay in the envelope, and it does
not make a process that is still running yield. A newcomer is admitted beside
the live processes when:

- every live reader can read what the newcomer migrates to, and it can read theirs;
- the oldest live writer keeps writing correctly through the newcomer's migrations;
- when the newcomer writes, it keeps writing correctly through theirs;
- the feature schemas are equal.

So a new build with the **same schema, layout and feature schemas** runs task, clock,
`migrate` and `workspace sync` commands while older `mcp serve`, dashboard and drain
processes stay up. A build that adds only **additive** migrations migrates the store
while they are live, and the older processes keep reading *and writing* it. Distinct
executables share the authority; identical copies trivially do.

#### A migration older processes cannot keep: the quiesce wait

A newer **writing** command whose migrations the live processes cannot keep (a
read-compatible or breaking migration beyond them) does not fail at once:

1. It records a **pending generation switch** in `.generation-pending.json` naming
   itself, its target identity and a deadline, and waits — `ORBIT_UPGRADE_QUIESCE_SECS`
   seconds, default 120.
2. While the switch is pending, no new process of the old generation is admitted;
   each is refused with `a generation switch is pending: pid N (role) is waiting until
   <deadline> to migrate to <identity>`. A command of the switch's own target identity
   waits behind it instead.
3. Live processes yield at their next safe point:
   - `orbit mcp serve` finishes its in-flight requests and exits (stdio closes; the
     client reconnects against the new generation);
   - the dashboard drains its connections and exits;
   - a pipeline worker completes its current top-level step, checkpoints it, records
     its run **`interrupted`** with error code `upgrade_quiesce` — not failed — and
     exits. Once the generation settles, the clock tick sweep resumes it
     automatically at most once from that checkpoint (`orbit job resume <run_id>`
     remains available for manual continuation). A claimed leaf on a follower is the
     exception: generic resume refuses it, so recover its claim on the owner and let a
     drain re-admit it. A drain coordinator yields between admission passes and admits
     no new leaves meanwhile; its already running leaves yield at their own step boundaries.
4. Once every participant is gone the switch takes the authority, records its own
   identity, and runs.

If the bound expires first, the command is refused and names every blocker:

```text
upgrade admission refused: a breaking migration is waiting (store schema: a live writer at
version 32 predates migration v33, which older writers do not keep), and these Orbit
processes did not yield within 120s: pid 41822 (drain, started 2026-09-27T21:33:02Z),
pid 40211 (mcp serve, started 2026-09-27T20:01:15Z), and any processes that did not
register (executable-generation-v1 binaries, or sandboxed children that cannot write the
Orbit root); leave the installation and stores unchanged. …
```

Quiesce those through their owners (or let long steps finish) and retry. An **older**
binary never displaces newer processes: it is refused as incompatible, and a
read-only command whose readers would break is refused the same way.

#### Handing a long-lived process over to a replaced executable

`orbit mcp serve`, the dashboard, and drain coordinators notice when the installed
executable they were started from is replaced (write beside it, rename over it — the
way `install.sh`, Homebrew and `orbit update` install). At an idle boundary they ask
the installed binary for its `update --contract` and, when it speaks
`compatibility-generation-v2` and the resume capability they need, re-exec it in place:

- `orbit mcp serve` hands over once no request is in flight. The pid, stdio pipes and
  MCP session survive — the client does not re-initialize — because the new image
  receives the initialize parameters and any unread input (`mcp-stdio-v1`).
  Only requests accepted by the MCP transport count as in flight; malformed or
  dropped messages cannot prevent handover when their errors omit the request id.
  A buffered partial line larger than 32 KiB defers handover: the current image
  keeps reading and serving the client, then retries at a later idle boundary.
- The dashboard drains, then execs the new image on the same address without
  reopening a browser.
- A drain coordinator (`drain-adopt-v1`) execs between admission passes and **adopts**
  its own run: same run id, owner pid and window, resuming from its checkpoints.
  Running leaves stay on the image they started with and finish normally.

The new image joins the authority like any newcomer, so an incompatible replacement
triggers the quiesce wait above instead. A candidate that cannot take over (older
contract, missing capability) is logged once, and the process keeps running the
replaced image until it exits. The `mcp listen` TCP listener and one-shot commands
are not handed over; they finish on the image they started with.

#### `--contract` and `--preflight`

`orbit update --contract --json` reports protocol support without opening state:

```json
{"schema_version":1,"contract":"executable-generation-v1","contracts":["executable-generation-v1","compatibility-generation-v2"],"admission_contract":"compatibility-generation-v2","compatibility":{"store_schema":{"version":32,"writer_floor":28,"reader_floor":26},"workspace_layout":{"version":3,"writer_floor":3,"reader_floor":3},"features":{"automation":3,"local_pull":1,"review":1}},"resume":["mcp-stdio-v1","drain-adopt-v1"]}
```

`contract` stays `executable-generation-v1` because every v2 binary still honours it
(see [Rollout beside executable-generation-v1 processes](#rollout-beside-executable-generation-v1-processes));
`admission_contract` is the protocol it admits by. The supported updater requires
this response from a candidate before installation; a missing or incompatible
protocol refuses, including a downgrade to an unprotected build.

`orbit update --preflight --json` is the wrapper-facing admission probe. It
opens no runtime, migrates no store, downloads nothing, and changes no binary
or managed resource. It uses OS locks under every generation authority the
invocation can be refused by, in the order the update takes them:

1. The invocation's own resolution — `--root`, then `ORBIT_ROOT`, otherwise the
   host-global root (normally `~/.orbit/`; managed children retain their
   supplied registry root). This is the authority this process would pin as a
   client, and it is reported as `global_root`.
2. The host-global root as well, whenever a `--root` / `ORBIT_ROOT` override
   named something else. A root override does not move what `orbit update`
   replaces: the executable is the running one (`~/.orbit/bin/orbit` for a
   managed install), and every client started *without* an override — including
   persistent `orbit mcp serve` processes — joins the host-global root.
3. The initialized workspace that convergence would use, when that directory
   is a different authority. An initialized `--root` / `ORBIT_ROOT` selects it. An
   override that is not yet an initialized workspace is still probed as a
   generation root: `--preflight` does not require `orbit workspace init` on
   that path, and it includes a workspace discovered from the working directory
   when one exists. `orbit update` without `--preflight` still refuses that
   uninitialized explicit root before it converges. Spellings of the same
   directory are one authority.

Every distinct authority is listed in `admission_roots`, and a refusal names
the authority it came from. Exit 0 returns:

```json
{"schema_version":1,"admitted":true,"reservation":false,"contract":"executable-generation-v1","admission_contract":"compatibility-generation-v2","compatibility":{…},"quiesce_timeout_secs":120,"global_root":"/srv/project","admission_roots":["/srv/project","/home/operator/.orbit"]}
```

Exit 1 with `upgrade admission refused` on stderr means stop before installation;
`--json` emits the CLI's normal JSON error envelope on stderr. This is an observation,
**not a reservation**. `orbit update` itself still takes each authority **exclusively**:
it refuses while any participant is live or a switch is pending, retains admission
across staging and replacement, and pins the candidate's generation (with the
candidate's reported identity, so compatible builds may join once it releases). Use
`--preflight` to learn whether it would be admitted now; use an installer that renames
over the executable when long-lived processes should stay up and hand over instead.
`--check` only checks release availability and is not this probe.

Admission also requires each authority's `.generation.lock` to be *writable*, and
`orbit update` checks that before anything is downloaded, staged or replaced. An
authority that can never record a takeover — a `~/.orbit` on a read-only mount, or one
whose record another user owns — refuses `orbit update --root <scratch>` up front with
`the record cannot be written from here`, rather than replacing the binary and
returning `needs_recovery`.

Isolated `HOME=` relocates `~/.orbit` itself, which is what in-process MCP roundtrip
fixtures use, and it moves the host-global authority with it. A root override protects
*state*, not host-binary replacement: a read-only unpinned `~/.orbit` (the agent-executor
/ Cowork sandbox) cannot block `orbit --root <scratch> init`, because that invocation
joins only its own resolved root. `orbit update` and `--preflight` admit against the
host-global root too, and an environment with no resolvable home has no host-global
authority to observe, so `orbit update` refuses there rather than replacing blind.

A participant that can only read the admission files — a read-only mount, or a
sandboxed child denied writes under its authority root — still joins a compatible
generation, and a read-only command joins even when it cannot record itself in the
envelope. It can never record a takeover: a switch it would need is refused with `the
record cannot be written from here`, leaving the record intact. On macOS, a managed
child with `ORBIT_REGISTRY_ROOT` joins its parent's host authority and keeps global
stores on that registry even if it sets `ORBIT_ROOT` to select shared workspace data.

Admission grants no permission: workspace selection, operator/agent capability,
remote caller and managed-run checks all still run. On Linux the executable digest
comes from `/proc/self/exe`, including a deleted running inode; on macOS the native
Mach-O image UUID must match the loaded image before the opened descriptor is hashed.
Hashing a whole binary dominates a short command, so the digest is remembered in
`.generation-image-digest.json` (a disposable cache, safe to delete) keyed by the
image's device, inode, size, mtime and ctime. Any rewrite or replacement changes the
key and is re-hashed; an image modified within the last two seconds is never
recorded, so a same-tick rewrite cannot alias an earlier entry. A read-only
participant reads the cache but never writes it, so a read-only join leaves the
root byte-identical.
OS locks release on exit or crash (and on exec, which is how a handover leaves);
never unlink `.generation.lock`, `.generation-admission.lock`,
`.generation-compat.json`, `.generation-pending.json` or `.generation-participants/`
to force admission. Keep them in the authoritative root and out of lock-file garbage
collection. A participant record left by a process that exited without cleanup is
unlocked, and the next admission collects it.

A new writing `orbit clock tick` that is refused — the live generation is
incompatible, or a switch is pending — logs one dated hold summary and runs again
once admitted. A claimed worker terminated without a recorded cancellation (an
external installer or service restart signalling it) is `interrupted` with
`worker_terminated` whether its supervisor observes SIGTERM or stale-owner
reconciliation sees the dead process first.

#### Rollout beside executable-generation-v1 processes

Processes from builds that predate this contract admit by executable digest alone,
and never write `.generation-compat.json`. The two protocols share
`.generation.lock`, so during a rollout:

- While a v1 process holds the authority, a v2 **writer** with a different digest is
  refused at once (`another executable generation is still running (this command
  writes; …)`), exactly as v1 would
  refuse it — v1 processes do not yield, so waiting would only delay the refusal.
  A v2 **read-only** command joins a v1 generation when its store schema equals the
  store's, as v1 read-only joins did.
- A v2 process that takes an idle authority records both the digest (for v1) and its
  identity. A v1 process with another digest is then refused by the digest check, as
  before; a v1 process that takes an idle authority rewrites the digest, which
  invalidates the v2 envelope, and later v2 newcomers fall back to the v1 rules until
  the authority is idle again.
- `orbit update` keeps speaking v1 to the updater: the candidate must report
  `contract: executable-generation-v1`, and its `compatibility` is recorded only when
  it also reports `admission_contract: compatibility-generation-v2`.

**Bootstrap limitation:** processes from before generation admission existed at all
do not participate. Before the first protected upgrade, quiesce every such backend
(including unmanaged or pinned executables), install, and reconnect using the same
configured authority. Restarting a desktop window is not evidence that its backend
exited. Admission is not a downgrade waiver: normal schema and layout compatibility
checks stay in force.

### Recovery and resumption

Re-running `orbit update` is the resume. At the installed version it skips the replacement and
re-runs the same idempotent convergence steps, so a run that failed at `migrate --confirm`,
`workspace sync`, or `clock repair` is finished by running it again — or by running that one
command directly and reading its diagnostics. `clock repair` fails when the unit manager
refuses to reload the rewritten unit; it names the `launchctl`/`systemctl` command to run. When `--root` or `ORBIT_ROOT` selected the workspace, recovery output
includes that root explicitly, so retrying from a different checkout does not silently switch the
workspace being repaired.

The outgoing executable stays at `<orbit>.previous`. Restoring it is safe when the state it
must open is newer only by additive migrations — it keeps reading and writing — or by
read-compatible ones — it serves reads and refuses writes — and refused when a breaking
migration separates the two. See
[Run an older binary against a newer workspace](#run-an-older-binary-against-a-newer-workspace).

Without a root override, `orbit update` converges **the workspace you run it from**. `ORBIT_ROOT`
selects an environment-only override, while an explicit `--root` takes precedence over it. Run
the update (or `orbit migrate --confirm` and `orbit workspace sync`) for each other registered
workspace after upgrading, and restart long-lived Orbit services and pipeline workers so newly
dispatched agents inherit the replacement build.

### Downgrades

A release older than the **currently installed** binary — re-read under the update lock, not
the version the running process started with — is refused unless `--allow-downgrade` is passed.
Even then, the staged older binary must be able to open this workspace's state — `orbit update`
runs its `migrate --dry-run --json` *before* replacing anything and requires an
explicit up-to-date report with matching current/supported layout and schema versions.
Missing reports and forward-compatible success (newer state the older binary could still
open) both refuse replacement.

### Release mirrors

`ORBIT_UPDATE_RELEASE_DIR` points `orbit update` at a local mirror instead of GitHub Releases,
for air-gapped or staged rollouts. The layout is `latest-version.txt` plus
`v<version>/{orbit-<target>.tar.gz,orbit-checksums.txt,orbit-checksums.txt.sig}`. Signature and
checksum verification are unchanged — a mirror does not lower the bar. `ORBIT_INSTALL_REPO`
selects a different GitHub repository, as it does for `install.sh`.

`orbit update` verifies the manifest with the compiled release trust set unless
`ORBIT_RELEASE_TRUSTED_KEYS_FILE` is set and
`ORBIT_RELEASE_TRUSTED_KEYS_FILE_ACKNOWLEDGE_TRUST_CHANGE=1`. Records are the same
`id|not_after|revoked_at|public_key_path` lines `install.sh` reads: blank lines and
`#` comments are ignored, and a relative public-key path is resolved next to the
record file. A key whose signature matches is rejected when `revoked_at` is set,
even if `not_after` is still in the future. `orbit update` does not honor
`ORBIT_RELEASE_PUBLIC_KEY_FILE` by itself; setting it together with the trust file
is an error. Without the acknowledged file, only the compiled keys verify.

Both HTTP releases and local mirrors limit each input before buffering it: latest-release
metadata and `latest-version.txt` to 64 KiB, the checksum manifest to 1 MiB, its detached
signature to 16 KiB, and the compressed archive to 256 MiB. An input over its limit fails
before the installed executable is replaced, including when an HTTP server omits
`Content-Length`. The extracted executable has a separate 256 MiB limit.

### Deploy a locally built candidate pinned to a source commit

When a fix must reach a host before it is released, install a build of an exact
source commit through the same guarded replacement instead of copying a binary over
the installed one. `--preflight` followed by a raw copy is not an install: it gives up
admission between the probe and the copy, keeps no backup and converges nothing.

```sh
set -eu
SHA='replace-with-the-full-40-or-64-hex-commit'
WORKSPACE='/absolute/path/to/the-intended-workspace'
test -z "$(git status --porcelain)" # start in a clean Orbit source checkout
git fetch --all
git cat-file -e "$SHA^{commit}"
git checkout --detach "$SHA"
test "$(git rev-parse HEAD)" = "$SHA"
test -z "$(git status --porcelain)"
cargo build --release --locked -p orbit-cli
C="$(pwd -P)/target/release/orbit"
"$C" update --local-candidate "$C" --source-commit "$SHA" \
  --write-candidate-manifest ~/orbit-candidate-"$SHA".json
# Quiesce Orbit clients (see below), then run from the intended workspace so
# discovery and convergence use that workspace rather than the source checkout.
cd "$WORKSPACE"
"$C" update --local-candidate "$C" --candidate-manifest ~/orbit-candidate-"$SHA".json \
  --source-commit "$SHA" --install-target ~/.orbit/bin/orbit --json
```

**Trust is `operator_attested`.** The manifest records what the operator asserts — the
source commit — beside what Orbit can compute: the candidate's SHA-256 and its target
triple read from the executable header. Orbit cannot prove the bytes were built from
that commit, so the report says `trust: operator_attested`, `signed_release: false`, and
gives each field's evidence (`operator_attested`, `computed_from_accepted_bytes`,
`executable_header`). Build from a clean checkout of the pinned commit and keep your own
build evidence. Each platform builds its own candidate: two hosts on the same commit
share source, not bytes. The release path, its signature verification and trusted keys
are unchanged; a local candidate never satisfies them.

The manifest (`kind: orbit-local-candidate`, `schema_version: 1`) is written once —
an existing path is never overwritten — and abbreviated commits are refused.

**Bootstrap.** Run the *candidate's* `orbit update`, not the installed one: an installed
build that predates `--local-candidate` cannot install it, and the candidate never
assumes it is the install target. `--install-target` is required and names the managed
executable (`~/.orbit/bin/orbit`, or `$ORBIT_INSTALL_DIR/orbit`). It is refused when
it is not named `orbit`, is a symbolic link or not a regular file, is not owned by the
invoking user, or is owned by a package manager (npm, Homebrew, `cargo install`, a
checkout build) or an unknown install. Its identity (device and inode) is re-checked
under the update lock and again immediately before the swap.

Then the ordinary update order applies, with the candidate in place of a download:

1. Inspect the install target, acquire generation admission for the invocation,
   host-global, and selected workspace roots (each distinct authority is locked;
   a live client on any of them refuses the update), then acquire the
   install-directory lock. Workspace discovery follows the current directory
   independently of `HOME`: run from the intended workspace, and for isolated
   smoke checks use an isolated checkout as well as isolated `HOME` and install
   target. The JSON report's `workspace_root` and `admission_roots` show the
   selected workspace and every authority held through convergence.
2. Stream the candidate into the staging file beside the target (1 GiB limit) and hash
   the staged copy. Those are the accepted bytes: replacing or rewriting the candidate
   path afterwards does not change what is installed.
3. Require the manifest's commit to equal `--source-commit`, its digest and target to
   equal the accepted bytes, and the target to match the installation. A candidate
   rebuilt after its manifest was written is refused.
4. Require the staged candidate's admission contract and version, and apply the
   [downgrade](#downgrades) rules with `--allow-downgrade`. An **equal** version with a
   different digest is a replacement, not `already_current`.
5. Back up to `<orbit>.previous`, swap atomically, confirm the installed digest and
   version, pin the candidate's generation, then run the convergence steps as the
   installed candidate.

Every refusal before step 5 leaves the executable, its backup, the generation record
and every store untouched.

**Clients.** Live stdio MCP sessions, the dashboard, clock ticks and drain coordinators
make the update refuse with `upgrade admission refused`; they keep running on the
installed build and a claimed run is not interrupted or reset. Stop them (close the
MCP client or its window and confirm the backend exited, stop the dashboard, pause the
clock, let or cancel drains finish), run the update, and reconnect: they start from the
installed candidate.

**Replay and recovery.** Re-running the same command once the target already holds the
accepted digest skips replacement, re-pins and re-runs convergence (`outcome:
already_current`); that is how an interrupted run finishes. A failure after the swap
exits `4` with `outcome: needs_recovery`; the recovery text and
`local_candidate.retry_command` carry the exact command, including `--root` when one
selected the workspace. The report also lists `admission_roots`, the before and after
installed digests, and `release_source: local candidate (operator_attested, not a
signed release)`.

## Understand the version ledgers

Two ledgers guard `.orbit/` state and auto-apply on workspace open:

- **Workspace layout:** `.orbit/state/layout.version` plus an ordered migration registry in
  `crates/orbit-store/src/workflow/layout/`. A missing marker means a pre-versioning workspace and is adopted
  as v1. Upgraders serialize on `state/layout.lock`.
- **Store schema:** the `schema_meta` ledger table inside `orbit.db`, backed by
  `crates/orbit-store/src/driver/sqlite/migration/`. Each migration and its ledger row commit in one
  transaction.

Each ledger also carries a compatibility record written by whichever binary applied the
last migration — `.orbit/state/layout.compat` and the `migration.compat` row in
`schema_meta`. That record is what lets a binary older than the workspace decide whether
it may still read it; see
[Run an older binary against a newer workspace](#run-an-older-binary-against-a-newer-workspace).
Both files are Orbit-owned state: read them for diagnosis, never edit them.

The host task registry has a separate reader-compatibility marker:
`PRAGMA user_version` in `~/.orbit/tasks/index.sqlite` (or the configured global
root). Its v5 task/allocator format also supports the additive `task_action_keys`
table. The repaired executable ensures that table when opening a writable v5
registry, without raising the reader-compatibility floor. A complete v5 registry
can still open read-only; missing additive storage requires a writable open.

### Recover a task registry marked version 6

A previous build marked this additive table as registry v6, causing v5 readers
to fail even on ordinary workspace/task access. A build containing the repair
recognizes the shipped compatible v6 columns, keys, indexes, and allocation
constraints, then restores the v5 marker in a transaction. Existing task data,
allocator state, and permanent action reservations remain intact. Repeated or
concurrent recovery is safe. Unknown schema versions or unrecognized v6 shapes
remain refused; recovery never means discarding tables or task data.

Executable upgrade and database recovery are separate steps. An already-installed
old executable does not learn a future migration. Install a build containing this
repair through its owning install channel, then invoke **that exact executable**
to open the registry. No manual SQL or registry reset is needed. `orbit migrate`
reports workspace layout and store-schema migrations; its dry-run report is not
an inventory of task-registry additive setup.

For mixed macOS installations, inspect the paths before rollout:

```sh
type -a orbit
command -v orbit
ls -l /opt/homebrew/bin/orbit
/opt/homebrew/bin/orbit --version
~/.cargo/bin/orbit --version
```

For example, `/opt/homebrew/bin/orbit` may still point to
`Cellar/orbit/0.19.0` while `~/.cargo/bin/orbit` is a different checkout build.
Building the latter does not upgrade Homebrew or change shell command selection.
A version string alone may not distinguish two checkout builds; confirm the
selected executable was built from a revision containing the repair. Upgrade
Homebrew through Homebrew when a release containing the fix is available, or use
an explicitly selected, validated repaired build under the rollout owner's direction.

Quiesce the build that writes v6 and take a consistent backup of the registry
and canonical bundles using [the state inventory](./state-and-backup.md). Then
open through the repaired build on that host, for example:

```sh
/path/to/repaired/orbit workspace list
/path/to/repaired/orbit tool run orbit.task.show --input '{"id":"<real-task-id>","model":"codex"}'
```

After this supported open has recovered a compatible v6 registry, a v5 reader
can again read its existing workspaces and tasks. Before that open, an old v5
executable still refuses v6. Do not keep the defective v6 writer running: it can
raise the marker again. Align `PATH`, any explicit `ORBIT_BIN`, MCP/service
launch paths, and restarted workers with the intended executable. Other host
store/layout/host-config compatibility guards still apply; registry recovery is
not a guarantee that every older release can read every other store.

If recovery is refused, use the reported database and executable paths to check
which installation is running. Upgrade the selected executable or escalate the
unrecognized format to the rollout owner; never lower `user_version` manually.
This repair's automated fixtures run on Linux. macOS Homebrew/PATH behavior and
live host rollout require verification on the affected Mac by the rollout owner.

## Back up before a major upgrade

Stop or quiesce independently managed Orbit processes, then create consistent backups:

```sh
cp -a <workspace>/.orbit <workspace>/.orbit.bak
sqlite3 ~/.orbit/orbit.db "VACUUM INTO '/backups/orbit.db'"
```

See [Inventory and protect Orbit state](./state-and-backup.md) for WAL-safe alternatives and
the complete authoritative-state inventory.

## Review and apply migrations

```sh
orbit migrate --dry-run    # list pending without applying; exit 1 when any are pending
orbit migrate              # same safe inspection default
orbit migrate --confirm    # open the workspace, auto-apply, and report
orbit migrate --json       # machine-readable inspection report
```

Example dry run on a pre-upgrade workspace:

```text
$ orbit migrate --dry-run
│ COMPONENT          CURRENT   SUPPORTED │
│ workspace layout   0         3         │
│ store schema       0         17        │
Pending migrations:
  layout v1 (baseline) — adopt the versioned .orbit/ layout (records the current shape; changes nothing)
  layout v2 (archive-friction-tasks) — rewrite removed friction statuses as archived
  layout v3 (remove-task-checkout-projections) — remove verified legacy .orbit/tasks symlinks without following them or touching canonical task bundles
  schema v1 (baseline) through schema v20 (invocations_ts_index)
error: execution failed: 3 migration(s) pending; run `orbit migrate --confirm` to apply
```

Bare `orbit migrate` and the compatibility-explicit `--dry-run` form inspect without opening
the runtime. Applying pending migrations always requires `--confirm`; the command never prompts
or reads stdin.

Before applying layout v3, pause admissions and stop every older Orbit process
that can write tasks. Install the new binary, apply or trigger the workspace
upgrade, and only then restart workers. Older binaries still contain the
retired projection writer and can recreate links while they remain running.

## Run an older binary against a newer workspace

A binary older than the workspace no longer fails every command on the version number
alone. Each migration declares what it means for a binary that does not have it, and
the binary that applies a migration records that classification beside the version it
stamps — `state/layout.compat` for the layout, the `migration.compat` row in
`schema_meta` for the database:

- **additive** — it only adds state an older binary can both read *and keep writing
  through its own code paths*: new tables no existing row depends on, nullable or
  defaulted columns, indexes an older writer cannot violate, files it never touches.
  Rows the older binary writes afterwards stay correct for the newer one.
- **read-compatible** — an older binary still reads the result correctly, but its
  writes would not be: a `NOT NULL` column without a default, a constraint or trigger
  its statements could trip, a projection or journal the newer binary keeps in step
  with rows an older writer would not update, or a backfill an older writer would write
  back in the old shape.
- **breaking** — it removes, renames, or reinterprets state older binaries use.

An older binary reads the record and takes one of three paths. The same
classification decides upgrade admission: only additive migrations let older processes
keep writing beside a newer one (see
[Upgrade admission](#upgrade-admission-compatibility-generations)). The contract is
described in [docs/design/state-compatibility](../design/state-compatibility/2_design.md).

### Additive-newer: reads and writes

When every migration above the binary's supported version is additive, the workspace
opens normally and the older binary keeps reading and writing it. It never migrates or
restamps the newer state. `orbit migrate` reports the case as a successful inspection:

```text
forward-compatible: store schema version 33 is newer than this binary's supported
version 32, but only by migrations older writers keep; opened for reads and writes

This workspace is newer than this binary, by migrations older writers keep writing
through: commands read and write it as usual.
```

`--json` reports it under `forward_compatible` with `"writable": true`.

### Read-compatible-newer: unaudited CLI reads

When nothing breaking sits above the binary's supported version but at least one
read-compatible migration does, the workspace opens **read-only**. `orbit task list`,
`orbit task show`, `orbit run history`, and `orbit search` work; every write is refused
with its own diagnostic, and the older binary never migrates, restamps, or otherwise
rewrites the newer state:

```text
error: schema migration failed: cannot open a write transaction: this orbit binary
supports store schema version 20 and the store records version 21, so it was opened
read-only; reads are served normally — upgrade orbit to write to this store
```

`orbit migrate` (and `--dry-run`) report this as a successful inspection and name it:

```text
forward-compatible: store schema version 21 is newer than this binary's supported
version 20, but only by read-compatible migrations; opened read-only

This workspace is newer than this binary, by read-compatible migrations: read-only
commands work and writes are refused. Upgrade orbit to write to it.
```

MCP `tools/call` is **not** an unaudited read: even `orbit.workspace.list` must write
its durable audit event. An old process with a read-only newer store is therefore not a
usable MCP authority, even when `orbit task show` works at the CLI. Audit events are
writes, so a read-only command records no audit row and prints a `failed to write audit
event` warning.

The workspace layout has no single write choke point, so it cannot be held read-only:
a read-compatible layout migration the binary lacks refuses the workspace like a breaking
one. Records written by a binary from before writers were classified carry no
read-compatible list; an older binary then opens a newer store read-only and a newer
layout for writes, exactly as it did before.

### Breaking-newer, or unclassified: still refused

A breaking migration the binary lacks refuses the open, now naming that migration:

```text
error: schema migration failed: workspace '….orbit' has .orbit layout version 4, newer
than the newest version this orbit binary supports (3); migration v4 (relocate-run-state)
is a breaking change this binary does not have; upgrade orbit to open this workspace
```

State written **before** this contract shipped carries no compatibility record, and so
does state whose record is stale (a crash between stamping the version and writing the
record) or unreadable. All three refuse with the reason named, exactly as every newer
version did previously:

```text
error: schema migration failed: workspace '….orbit' has .orbit layout version 99, newer
than the newest version this orbit binary supports (3); it records no forward-compatibility
metadata, so this binary cannot tell whether the newer migrations are additive; upgrade
orbit to open this workspace
```

In every refusing case the remedy is the same as before: upgrade the binary that is
reporting it — through its own install channel — and re-run. Never hand-edit
`layout.version` or `layout.compat`, and never delete the record to force an open: that
converts a refusal into a binary operating on state it cannot interpret.

## Lexical search migration

Search now uses SQLite FTS5 BM25 only. `orbit semantic` (install, uninstall,
stats, index), `orbit search --hybrid`, `orbit search similar`, and the
`orbit.semantic.*` tools are removed. `orbit.search` rejects `semantic` and
`hybrid` inputs. Use distinctive query terms and `orbit search reindex` to
rebuild task chunks after imports or restores.

The search database retains its `state/semantic.db` filename so existing
backup and sandbox paths still work. On the first writable open, Orbit preserves
`chunks` and `corpus_fts`, migrates older inline FTS tables, drops the obsolete
`embeddings` table and its indexes plus `id_allocations`, and runs `VACUUM` to
reclaim disk space. Read-only opens do not migrate. Task create/update/delete
maintain chunks synchronously; `orbit doctor` reports `search-index` counts.

Stop older Orbit writers and upgrade/restart MCP, dashboard, and pipeline
processes together. The index migration is forward-only; older binaries must
not write the migrated database. Align PATH and explicit `ORBIT_BIN` settings
with the installed release before restarting those processes.

The `orbit-search-companion` binary and downloaded models are no longer used
or shipped. After stopping old processes, inspect `~/.orbit/embed/`, then
manually remove that dedicated directory (including `bin/`, `models/`, and the
active-model file) to reclaim the former model downloads. This does not affect
task records or the lexical index. No automatic deletion of that global
directory is performed.

Legacy `[semantic]` configuration and `search.model` are ignored with a warning
for the removal release; delete them from config.toml. Config get/set reject
these retired keys with migration guidance. The runtime no longer honors
`ORBIT_SEARCH_COMPANION*` environment overrides.

## Verify the upgrade

`orbit update` performs steps 1–3 below for the workspace it runs in. Do the same by hand when
a package manager owns the binary, and run steps 2–5 in every other registered workspace:

1. Replace or upgrade the binary.
2. Run `orbit migrate` (or `orbit migrate --dry-run`) to review pending layout/store changes,
   then `orbit migrate --confirm` to apply them.
3. Run `orbit workspace sync` to apply the provenance-safe managed-artifact actions. Operator
   edits, user-authored name collisions, and existing routine `name`/`hosts` bindings are
   preserved and reported with their paths. A routine that differs from a template a prior
   release shipped only in the settings Orbit's own surfaces change — `enabled`, the retired
   `hosts:` key — is not an operator edit: it retires or refreshes (keeping its `enabled`
   setting) so the upgrade converges without moving files by hand. `orbit workspace sync
   --check` reviews the same actions read-only and exits nonzero when managed artifacts need
   convergence. Diagnostics that belong to no single action — a provenance manifest Orbit could
   not write because the catalog is read-only or permission-denied, or untracked legacy YAML left
   in place — are listed as warnings (the `warnings` array in `--json`), and the run reports
   that artifacts are not fully converged. A `migrated` action under a skipped manifest write
   says its provenance was not recorded; make the catalog writable and rerun the sync.
4. Run `orbit doctor` and require all relevant checks to pass.
5. Restart any independently managed dashboard process after swapping the binary.

These commands are intentionally independent. `workspace sync` converges local definitions
embedded in the installed binary; it does not install a newer binary, pull a repository, sync
another host, or run general layout/store migrations. `workspace init` remains the one-time
registration/bootstrap operation, while `doctor` diagnoses health and performs only its named,
narrow repairs.

Agent subprocesses inherit an `ORBIT_BIN` pinned to the Orbit executable that dispatched
them, and that executable's directory is placed first on their `PATH`. An operator-set
`ORBIT_BIN` takes precedence. After replacing `~/.orbit/bin/orbit`, restart long-lived Orbit
services and pipeline workers so newly dispatched agents inherit the replacement build. Verify
both the explicit path and the ordinary command resolve the same tool-capable binary:

```sh
~/.orbit/bin/orbit tool run orbit.task.show --input '{"id":"<real-task-id>","model":"codex"}'
command -v orbit
orbit tool run orbit.task.show --input '{"id":"<real-task-id>","model":"codex"}'
```

If `~/.orbit/config.toml` uses settings newer than an installed binary supports, upgrade and
restart that binary. Never edit `schema_version` downward to bypass the guard.

See [Check Orbit health](./health-checks.md) for `orbit doctor` and dashboard-readiness
semantics. If verification fails, stop writers and restore the backups before further recovery.
