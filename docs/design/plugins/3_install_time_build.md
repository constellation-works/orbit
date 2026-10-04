---
type: design
title: "Threat model: install-time spec.build for source-built plugins"
summary: "Threat model and binding decisions for an opt-in spec.build that builds a git+ plugin source at install time: sandbox profile, network, environment, write scope, artifact digest, consent, pin files, doctor and abuse cases"
owner: claude
status: Draft
tags: [plugins, security, sandbox, supply-chain, install]
paths: ["crates/orbit-tools/src/plugin/source.rs", "crates/orbit-types/src/plugin/pin.rs", "crates/orbit-core/src/application/plugin/install.rs", "crates/orbit-core/src/application/plugin/inspect/doctor.rs", "crates/orbit-exec/src/linux_sandbox/**", "crates/orbit-exec/src/linux_landlock/**", "crates/orbit-exec/src/macos_sandbox/**"]
related_features: [plugins, policy-sandbox]
related_artifacts: [ORB-12878, ORB-12843, ORB-12874, ORB-12816]
last_updated: 2026-10-04
last_validated: 2026-10-04
---

# Threat model: install-time spec.build for source-built plugins

Status: decided, not implemented. No `spec.build` exists in the manifest schema today. This
document fixes the contract the implementation must follow. A change to any decision below is a
security decision and needs a change to this document in the same PR.

Builds on [1_scope.md](./1_scope.md) §3 (plugin sources, pin file, staged install) and §4.1–§4.3
(grants, execution protocol, backend sandbox).

## 1. What is being added

Today a plugin reaches a host as a directory, a `git+` checkout, a local archive, or an
`https://` archive pinned by `sha256:` digest (`crates/orbit-tools/src/plugin/source.rs`). In
every form the backend executable is already present in `.orbit-plugin/`. A Rust or Node
backend must therefore be published as a prebuilt archive per platform.

`spec.build` lets a manifest instead say how to produce the files its backend needs from the
repository it ships in:

```yaml
# .orbit-plugin/plugin.yaml
spec:
  build:
    programs: [cargo]                       # resolved at consent, like requires.programs
    fetch: [cargo, fetch, --locked]         # optional; the only phase with network
    command: [cargo, build, --release, --offline, --locked, --target-dir, "{{build_dir}}/target"]
    outputs:
      - from: target/release/orbit-graph    # relative to {{build_dir}}
        to: bin/orbit-graph                 # relative to the plugin root
    timeout_ms: 1200000
```

`fetch` and `command` are argv arrays and are never passed through a shell, so what is displayed
at consent is what runs. The only template available in them is `{{build_dir}}`.

Building runs code the plugin author controls (the build command, build scripts such as
`build.rs`, package install hooks, procedural macros) on the operator's machine, at a moment the
operator chose to install something. That code is the threat. A prebuilt archive pinned by
digest stays the recommended form. `spec.build` exists for plugins that cannot reasonably
publish one.

## 2. Assets, actors and trust boundary

**Assets the build must not reach:**

- Provider credentials and tokens in the environment and in well-known files (`GH_TOKEN`,
  `ANTHROPIC_API_KEY`, `~/.ssh`, `~/.aws`, `~/.config/gh`, cargo and npm publish tokens).
- Orbit authority: the global root (`orbit.db`, `config.toml`, `plugins/.grants/`,
  `state/plugin-secrets/`, `state/plugin-callbacks/`), callback credentials, and
  `ORBIT_OPERATOR`, `ORBIT_WORKSPACE_CLAIM_TOKEN` and the other `ORBIT_*` envelope names.
- Every other installed plugin's tree and state, workspace checkouts, and Git hooks.
- Host integrity: anything outside the build directory, including the installed plugin tree
  until Orbit itself publishes it.
- Services on loopback and the home network (the Orbit dashboard, other daemons).

**Actors:**

- *Plugin author, or anyone who controls the repository or its hosting:* controls the build
  command, the source tree and the lockfile at whatever commit the operator installs.
- *Dependency publishers and registries:* control what `fetch` downloads, within what the
  lockfile admits.
- *Workspace committers:* control `.orbit/plugins.yaml`, so they can name a source and a
  digest but must not be able to cause a build.
- *Agents and unattended automation:* managed workers, routines, auto-tasks, jobs, MCP callers
  and plugin backends. None may start a build or supply consent.
- *Operator:* the only actor who can authorize a build, one install at a time.

**Trust boundary.** Consent means the operator accepts that this command, at this commit, runs
in the build sandbox. It does not mean the operator trusts the author with the host. The build
sandbox must hold even when the build command is hostile. The built backend is not trusted
because it was built under a sandbox. It runs under the backend sandbox and the grants given at
`enable` (§4.3 of the scope), exactly as an archive-installed backend does.

## 3. Decisions

### 3.1 Sources that may build

**Decision.** Only a `git+` source pinned to a full commit object id (`#<40 or 64 hex>`) can
build. Branch and tag refs, directories, local archives and `https://` archives never build.
For those forms the manifest's `spec.build` is not executed. The install requires every
declared `outputs[].to` to already exist in the plugin root as a regular file, so a prebuilt
archive can carry the same manifest. Orbit fetches the commit into its own scratch directory
and checks that `HEAD` is the requested object before reading the manifest.

**Rationale.** A branch or tag can move between the time the operator reads the plan and the
fetch, and a local directory is writable by whatever else runs as the operator. A commit id in
the command line makes the reviewed input and the built input the same bytes. A source that
does not build keeps today's semantics, so one manifest serves both release archives and source
builds.

### 3.2 Execution profile

**Decision: a dedicated build profile, deny-by-default for reads, on both platforms.** The build
does not reuse the agent profile (host readable, writes confined) or the backend profile (grants
from the manifest). Both phases run under one profile named in the build record
(`linux-bwrap-build-v1`, `macos-sandbox-build-v1`). A host that cannot apply it refuses the
build and writes nothing. There is no fallback to a weaker profile and no `unsandboxed`
equivalent for builds.

The readable set is:

- the host runtime table the Landlock backend profile already uses (`/usr`, the dynamic loader,
  system libraries, resolver and CA files);
- each `spec.build.programs` entry, resolved once against the consenting operator's `PATH` to
  its canonical path, as for `requires.programs` (scope §4.3);
- the toolchain root of each resolved program, displayed at consent. A toolchain root is the
  installation directory the program runs from (for example a rustup `toolchains/<name>` and
  the `RUSTUP_HOME` settings file, or a Node install prefix). It is never `$HOME`, the global
  Orbit root, a workspace, or a path in `orbit-exec`'s `credential_paths` list. Orbit refuses
  to record a toolchain root that contains any of those;
- the build directory (§3.4).

The credential-path denies apply on top of the readable set and always win.

**Linux.** `bwrap` with a constructed root, not the agent sandbox's `--ro-bind / /`. Each
readable path is a `--ro-bind`; the build directory is the only `--bind`. `--unshare-all`
(user, pid, ipc, uts, cgroup and network namespaces), `--new-session`, `--die-with-parent`, a
fresh `/proc` and a minimal `/dev`. The build phase adds no `--share-net`, so it runs in an empty
network namespace with only its own loopback. Paths outside the readable set do not exist inside
the sandbox at all. A host where unprivileged user namespaces are unavailable (`probe_bwrap`
fails) refuses builds.

**macOS.** `sandbox-exec` with a new deny-by-default profile:
`(deny default)`, `process-fork` and `process-exec` limited to the readable set,
`file-read*` limited to the readable set, `file-write*` limited to the build directory, the
credential denies, and `(deny network*)` for the build phase. It is not a variant of
`compile_macos_sandbox_profile`, which starts from broad read access.

**Rationale.** The backend profile is shaped by grants the operator gives a known plugin. A
build runs before any grant exists and has no reason to read anything outside its toolchain and
its own directory. Read confinement is what makes the remaining network exposure
(§3.3) tolerable, because there is little worth exfiltrating. On Linux, bwrap's namespaces add
what Landlock cannot provide: an empty network namespace that also blocks UDP and host
loopback, and a pid namespace whose teardown kills every descendant (§4, leftover processes).

### 3.3 Network

**Decision: two phases, network only in `fetch`, and only outbound TCP to port 443.**

| Phase | Runs | Network | Writable |
|---|---|---|---|
| `fetch` (optional) | `spec.build.fetch` | Outbound TCP to remote port 443, plus name resolution. No listening, no other ports | `{{build_dir}}` |
| `build` | `spec.build.command` | None | `{{build_dir}}` |

- **Linux `fetch`:** bwrap with `--share-net` plus a Landlock ABI 4 network ruleset applied
  inside it that handles `bind` and `connect` for TCP and allows only `connect` to port 443. A
  kernel below ABI 4 refuses a manifest that declares `fetch`. A build-only manifest still runs.
- **macOS `fetch`:** `(allow network-outbound (remote tcp "*:443"))`, the mDNSResponder socket
  for resolution, `(deny network-outbound (remote ip "localhost:*"))`, and no
  `network-inbound` or `network-bind`.
- Orbit fetches the plugin source itself, before either phase, with the existing hardened
  `git clone` (scope §3). The build never sees `.git` or the source URL's credentials.
- Private registries are not supported. No registry credential, `.netrc`, `.npmrc` auth line or
  `CARGO_REGISTRIES_*` token is ever made available (§3.5), so a dependency that needs one fails.

**Rationale.** Most real builds need dependencies, and vendoring an entire crate or npm tree into
the plugin repository would make `spec.build` useless for the plugins it exists for. Splitting
the phases keeps the code that runs longest and is hardest to review (compilers, build scripts,
procedural macros) offline. Only the command that downloads gets a network. Port 443 is the one
port registries and HTTPS Git hosts need. Restricting to it keeps the dashboard and most
loopback or LAN services out of reach on Linux, where Landlock has no address filter. Dependency
integrity comes from the lockfile in the pinned commit, enforced by the toolchain's locked mode.
Orbit does not parse lockfiles, so it does not claim to verify them (§4, dependency confusion).

### 3.4 Write scope and what gets installed

**Decision.**

- **Build directory.** A fresh directory created with a random name under the namespace's
  staging area, `~/.orbit/plugins/<ns>/.build-<nonce>/`, owned by the operator, mode `0700`,
  created one component at a time with no links followed. It holds `src/` (a writable copy of
  the fetched checkout, without `.git`), `home/`, `tmp/` and whatever the phases create. It is
  the only writable path in either phase. Orbit's install pruning (scope §3) skips `.build-*`
  directories that belong to a live build, and removes them otherwise.
- **The pristine checkout stays outside the sandbox.** The fetched commit lives in Orbit's own
  scratch directory. The sandbox gets a copy. Orbit reads the manifest shown at consent, and
  copies the installed plugin root, from the pristine checkout, never from `src/`.
- **Only declared outputs cross back.** After a successful `build` phase, each
  `outputs[].from` must resolve inside `{{build_dir}}` to a regular file, physically, with no
  symbolic links, hard links, devices or FIFOs on the path. Each `outputs[].to` must lie inside
  the plugin root, must not be `plugin.yaml`, and must not replace a file present in the
  pristine plugin root. Orbit copies the file, keeps the owner-execute bit, and clears setuid,
  setgid and sticky bits. Everything else the build produced is discarded with the build
  directory.
- **Then the existing install path takes over.** The pristine plugin root plus the outputs are
  copied into the usual staging directory and published with the single `rename`, under the
  per-namespace lock (scope §3). The build itself runs before the lock is taken, as source
  resolution does today, so a long build never blocks another namespace's lifecycle operation.
- **Size caps.** The outputs together are limited to the archive unpack limit
  (`MAX_UNPACKED_BYTES`, 256 MiB). The build directory is limited to 8 GiB, measured by Orbit
  while the phases run (§4, resource exhaustion).

**Rationale.** The manifest, schemas, definitions, skills and tests the operator's consent
covers come from the reviewed commit, and a build script cannot rewrite them. Its only influence
on the installed tree is the declared output bytes, and those are digested (§3.6). Building under
`~/.orbit/plugins/<ns>/` keeps build scratch on the same filesystem as the install for the final
rename. It also sits under a tree that backend `fs.write` admission and the agent sandbox's write
inventory already refuse (scope §4.1, §4.3), so no backend or agent can tamper with a build in
progress.

### 3.5 Environment

**Decision.** The build starts from an empty environment. Orbit sets only:

```
PATH=<dirs of the resolved build programs>:/usr/bin:/bin
HOME={{build_dir}}/home     TMPDIR={{build_dir}}/tmp     LANG=C.UTF-8     TZ=UTC
SOURCE_DATE_EPOCH=<committer time of the pinned commit>
ORBIT_BUILD_DIR={{build_dir}}   ORBIT_BUILD_SRC={{build_dir}}/src   ORBIT_BUILD_PHASE=fetch|build
```

plus, when present in the operator's environment, a fixed Orbit-owned list of toolchain locator
names (`RUSTUP_HOME`, `RUSTUP_TOOLCHAIN`, `CARGO_HOME` rewritten to `{{build_dir}}/home/.cargo`,
`GOROOT`, `JAVA_HOME`). A locator is passed only when its value is inside a toolchain root shown
at consent.

- There is no `spec.build.env_pass` and no operator flag for adding variables.
- `ORBIT_*` names are never set beyond the three above. In particular, no callback descriptor or
  token, no `ORBIT_OPERATOR`, `ORBIT_ROOT` or `ORBIT_WORKSPACE`, and no run envelope.
- Provider and forge credentials (`GH_TOKEN`, `GITHUB_TOKEN`, `ANTHROPIC_API_KEY`,
  `OPENAI_API_KEY`, `AWS_*`), `SSH_AUTH_SOCK`, `GIT_ASKPASS`, `NPM_TOKEN` and
  `CARGO_REGISTRIES_*` are never passed, whatever the locator list later contains. A test pins
  this denylist against the locator list.
- `HOME` points into the build directory, so `~/.gitconfig`, `~/.npmrc`, `~/.cargo/config.toml`
  and `~/.cargo/credentials.toml` are absent instead of masked.

**Rationale.** An allowlist that starts empty cannot leak a credential added to the operator's
shell later. A manifest-controlled passthrough would let the author choose which secret to read.
The locator names exist only because toolchain managers find their installs through them. Fixed
`TZ`, `LANG` and `SOURCE_DATE_EPOCH` make reproducible builds possible, which the pin's artifact
digest depends on (§3.7).

### 3.6 Artifact digest and the build record

**Decision.** After outputs are copied into staging, Orbit computes:

- `sha256` of each output file;
- the **artifact digest**: `sha256("orbit.plugin.build.v1\n" + for each output sorted by
  `to`: "<to>\0<mode as octal>\0<sha256 hex>\n")`.

The plugin row gains a build record, mirrored into a host-owned file beside the grant witness,
`~/.orbit/plugins/.grants/<ns>.build.json`:

| Field | Value |
|---|---|
| `source` | the `git+` URL as given, credentials stripped |
| `commit` | the verified commit object id |
| `fetch`, `command` | argv exactly as run |
| `programs` | each name with its recorded canonical path |
| `toolchain_roots` | as consented |
| `profile` | `linux-bwrap-build-v1` or `macos-sandbox-build-v1`, plus the Landlock ABI used for `fetch` |
| `outputs` | `to`, mode and `sha256` for each output |
| `artifact_digest` | `sha256:<hex>` |
| `consent` | timestamp, OS user, Orbit version, and the literal `--allow-build` |
| `log` | path of the capped build log kept under `state/plugin-builds/<ns>/` |

The existing `manifest_digest` still covers `plugin.yaml`, which now includes `spec.build`.

**Rationale.** The scope's `manifest_digest` deliberately excludes the backend executable
(scope §4.1). For a built plugin the executable is exactly what the build produced, so it needs
its own digest. A domain-separated digest over sorted paths, modes and hashes is stable across
hosts and copy order, so two hosts that build the same commit reproducibly record the same
value. The witness directory is already outside every backend's and agent's write boundary
(scope §3, "The recorded grant set is tamper-evident").

### 3.7 Pin files

**Decision.**

- **A committed pin alone never triggers a build.** `orbit plugin sync`, and any install path a
  pin drives, never runs `spec.build`. When a pinned plugin is missing and its source would
  build, sync reports the entry unsatisfied, naming `orbit plugin add <source> --allow-build`.
  It writes, enables, links and seeds nothing for that entry, and continues with the other pins.
  `sync` has no `--allow-build` flag.
- **What a pin may record:** `source: git+<url>#<commit>`, which must be a full commit for a
  building source, and a new `artifact_digest: sha256:<hex>`. `artifact_digest` is allowed only
  on a `git+` source pinned to a commit. The existing `digest` field remains reserved for
  `https://` archives (`crates/orbit-types/src/plugin/pin.rs`).
- **A mismatch fails closed.** At `orbit plugin add|upgrade --allow-build`, when the current
  workspace pins that namespace with an `artifact_digest`, an output digest that differs refuses
  the install. Nothing is published, and the refusal shows both digests. At `sync`, an installed
  build whose recorded commit or artifact digest differs from the pin is unsatisfied. Sync does
  not enable, toggle on or seed it in that workspace, and `doctor` reports it. A differing pin
  never causes a rebuild.
- A pin is never consent of any kind. As with grants (scope §3), consent comes only from the
  operator's command line.

**Rationale.** Anyone who can commit to a workspace can edit `.orbit/plugins.yaml`. If a pin
could start a build, a pull request would be enough to run arbitrary code on every host that
syncs it. A pin is useful as a statement of expectation: which commit, and which bytes it should
produce. Checking that expectation can only refuse, never execute. An `artifact_digest` that
cannot be reproduced on another host makes that host's install fail, which is the intended
outcome. The remedy is a reproducible build or a prebuilt archive.

### 3.8 Operator consent

**Decision.**

- **Explicit and per install.** `orbit plugin add <git+url#commit> --allow-build` and
  `orbit plugin upgrade <ns> <git+url#commit> --allow-build`. Without the flag, a source whose
  manifest declares `spec.build` is refused before any phase runs, whether or not a terminal is
  attached. The refusal prints the build plan: source, commit, `fetch` and `command` argv,
  resolved programs, toolchain roots, phases with their network policy, limits and outputs.
  It writes nothing. The flag also prints the plan before running it.
- **Consent never carries over.** Each upgrade, each `--force` reinstall, and each change of
  commit needs the flag again, even when the command is unchanged. A widening upgrade's
  re-consent for grants (scope §4.1) is a separate decision and is not implied by
  `--allow-build`, nor the reverse. `add --allow-build --enable --grant …` records both in one
  command.
- **Not implied by anything unattended.** No config key, environment variable, pin entry,
  routine, auto-task, job, deterministic action or MCP tool can supply it. Orbit refuses
  `--allow-build`, with `build_consent_unavailable`, when the process:
  - carries a managed-run envelope (`ORBIT_MANAGED_RUN_CONTEXT`, `ORBIT_RUN_ID`) or runs inside
    an Orbit agent sandbox;
  - is a recognized plugin backend child (already refused for every command but tool calls,
    scope §4.2);
  - was dispatched by the clock, a routine, an auto-task or a job runner.
- **Layered, not only advisory.** A sandboxed worker that scrubs its environment still cannot
  complete an install. `~/.orbit/plugins/` is outside the agent sandbox's write inventory (scope
  §4.3), so the build directory and the published tree cannot be created.

**Rationale.** This mirrors the per-run consent of `orbit plugin test` (`--accept-requested` or
an explicit `--grant`, scope §5): consent is an argument to one command, never ambient state. A
single rule for terminal and non-terminal callers avoids a prompt that `yes |` could answer.
Binding consent to a commit id in the same command line means the reviewed plan and the
executed plan cannot diverge. Refusing managed contexts keeps the question with the operator.
An unsandboxed agent the operator runs by hand already has operator authority (§4, consent
laundering).

### 3.9 Doctor

**Decision.** `orbit plugin doctor`, and the plugin section of `orbit doctor`, report each
source-built plugin as an informational row, not a finding. The row shows the source and commit,
the `fetch` and `command` argv, the profile and Landlock ABI, the consent record (when, which OS
user, which Orbit version) and the recorded artifact digest. `orbit plugin show` prints the same
fields. Findings:

- the artifact digest recomputed from the installed outputs differs from the recorded one
  (the installed binary was modified after the build);
- the build record and its witness copy disagree, or one is missing for a row installed by a
  build. That row registers inactive at load, like a grant witness mismatch;
- the current workspace's pin names a different commit or `artifact_digest` (the same
  offline comparison as today's `archive_digest_drift_rows`). The finding names the
  `orbit plugin upgrade … --allow-build` command;
- a recorded build program or toolchain root that no longer exists. This is informational for an
  installed plugin and blocks only the next build;
- a leftover `.build-*` directory not owned by a live build.

Doctor never runs a build, contacts the source, or re-fetches anything.

**Rationale.** An operator auditing a host needs to see which installed code was produced on
the host from what, and under what consent, without reading SQLite. Drift checks are offline and
deterministic, matching the archive-digest finding that already exists.

## 4. Abuse cases

| Case | What the attacker does | Decision | Residual |
|---|---|---|---|
| Malicious build script | `build.rs`, a postinstall hook or the command reads secrets, plants persistence or attacks the network | Deny-by-default reads (§3.2), empty environment (§3.5), build directory as the only write root (§3.4), no network in `build` and port 443 only in `fetch` (§3.3), declared outputs only | It can produce a malicious backend. That backend is bounded by the backend sandbox and grants (scope §4.3), as an archive's is. During `fetch` it can send what it can read (source, toolchain) over 443 or UDP |
| Hostile output paths | `outputs[].from` symlinked to `~/.ssh/id_ed25519`, or `to` set to `../../bin/orbit` or `plugin.yaml` | Physical resolution with no links inside `{{build_dir}}`, `to` confined to the plugin root, no `plugin.yaml`, no overwrite of pristine files, special mode bits cleared | None beyond the hostile backend above |
| Dependency confusion | A public package shadows an internal name, or a registry serves altered bytes | No private-registry credentials or user registry config (§3.5); toolchain locked mode against the lockfile in the pinned commit; the `fetch` argv is shown verbatim at consent; optional pin `artifact_digest` | Orbit does not parse lockfiles. A source with no lockfile or no `--locked` gets whatever the registry serves at build time. The consent plan makes that visible, and an `artifact_digest` pin catches drift on later hosts |
| TOCTOU on the source | A tag or branch moves after review; the local tree changes mid-build; the build rewrites the manifest it was reviewed by | Full commit required and verified (§3.1); Orbit's own pristine scratch; the sandbox works on a copy; the installed root comes from the pristine copy (§3.4) | The repository host can refuse or delay the commit, but not substitute it, short of a SHA-1 collision on a SHA-1 repository |
| TOCTOU on toolchains | A toolchain link is retargeted between consent and run | Programs and roots resolved to canonical paths at consent and re-checked before each phase (same rules as `requires.programs`) | A toolchain modified in place, at the same path, is the operator's own install. Orbit trusts it as it trusts `git` |
| Resource exhaustion | Infinite loop, fork bomb, filling the disk, giant logs | `timeout_ms` capped by `PLUGIN_BUILD_TIMEOUT_CEILING_MS` (3,600,000), default 1,200,000 per phase; build directory capped at 8 GiB by polling, with a kill on breach; stdout and stderr captured to one log capped at 1 MiB (head and tail kept); `RLIMIT_CORE=0`; outputs capped at 256 MiB; pid namespace on Linux | Memory and CPU are not limited beyond the timeout. The kernel OOM killer is the bound, and the disk poll can overshoot by one interval |
| Leftover processes | A daemonized child outlives the build to keep a foothold | Linux: the pid namespace is torn down with bwrap (`--die-with-parent`, `--new-session`), which kills every descendant, even those that called `setsid`. macOS: a new process group and session, `killpg` with SIGTERM then SIGKILL at exit or timeout (`orbit-exec` supervision) | macOS has no pid namespace. A child that calls `setsid` escapes the group. It stays under the inherited sandbox profile (no network after `fetch`, writes only to a build directory Orbit then deletes), so it can burn CPU but cannot persist or exfiltrate further. The macOS build record names this containment level |
| Consent laundering | A pin, routine, auto-task, agent or plugin backend tries to start a build | §3.7 and §3.8: no unattended source of consent, refusal in managed contexts, the agent sandbox's write boundary | An unsandboxed agent with a shell, run by the operator outside Orbit, can pass the flag. It already holds operator authority |
| Replaying consent | An upgrade reuses yesterday's consent for a new commit | Consent recorded per commit; every install needs the flag (§3.8) | None |
| Build record tampering | A backend or agent edits the row to hide a modified binary | Witness copy under `plugins/.grants/`, outside every write boundary; mismatch makes the row inactive (§3.9) | As for grants, the witness is a digest, not a MAC. It holds because of the write boundary, not cryptography |

## 5. Limits this design accepts

- **The built code is as trusted as the author.** The build sandbox protects the host during the
  build. It does not make the output safe. Review, grants and the backend sandbox still decide
  what the plugin can do.
- **`fetch` has a network.** On Linux, UDP (DNS and anything else) and host loopback port 443 stay
  reachable during `fetch`, because Landlock filters only TCP by port. What can leave is bounded
  by what the sandbox can read.
- **Reproducibility is the author's job.** Orbit fixes time, locale and paths but does not
  require deterministic output. An unreproducible build is installable but cannot be pinned by
  `artifact_digest` across hosts.
- **No memory or CPU quota.** A cgroup v2 limit is a candidate addition. Builds are rare,
  consented and timed out, so it is not required for the first implementation.
- **No Windows.** Plugins already declare `platforms: [linux, macos]`.

## 6. Implementation checklist

The implementation task delivers, with boundary tests for each refusal:

1. `spec.build` in the manifest schema (`deny_unknown_fields`, argv arrays, `outputs` shape)
   and its `orbit plugin validate` checks.
2. Commit-pinned `git+` fetch with a `HEAD` check (§3.1).
3. The two build profiles and their probes, with refusal when either profile is unavailable
   (§3.2–§3.3), plus profile goldens in `make goldens`.
4. Build directory lifecycle, output copy and caps (§3.4), environment construction and the
   denylist test (§3.5).
5. Artifact digest, build record and witness (§3.6). Load refuses a missing or mismatched record.
6. Pin `artifact_digest` parsing and sync's refusal to build (§3.7).
7. `--allow-build` on `add` and `upgrade`, the plan output, and the managed-context refusal
   (§3.8).
8. Doctor and `plugin show` rows (§3.9), and updates to [1_scope.md](./1_scope.md) §3,
   `docs/CONFIG.md` (pin fields) and CLI goldens in the same PR.

## Task References

- ORB-12878: this threat model.
- ORB-12843: the plugin-source review it was split from.
- ORB-12874: digest-pinned `https://` archive sources, the recommended alternative.
- ORB-12816: the consent gate for `orbit plugin test`.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
