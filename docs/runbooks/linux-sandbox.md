---
type: runbook
summary: Explain automatic Linux Bubblewrap onboarding, distro eligibility, readiness, and native validation status.
tags: [operations, sandbox, linux, bubblewrap, apparmor]
paths:
  - "install.sh"
  - "crates/orbit-cli/src/command/init/**"
  - "crates/orbit-exec/src/**"
  - "crates/orbit-core/src/adapter/engine_host/v2_host/tests/**"
  - "crates/orbit-cmd/src/update/bundled_bwrap.rs"
  - "scripts/build-bundled-bwrap.sh"
related_features: [policy-sandbox, executors]
related_artifacts: []
last_validated: 2026-10-04
---

# Linux sandbox onboarding and diagnostics

The shell installer prepares the Linux host after installing the signed Orbit binary.
For npm and direct-binary installs, normal `orbit init` performs the same preparation;
npm postinstall does not run `sudo` or prompt. Run installation and `orbit init` as the
unprivileged account that will execute Orbit. An installer invoked through `sudo` uses
`SUDO_UID`/`SUDO_GID` to probe that account; a root invocation without an identifiable
unprivileged account stops before changing the host. The installer then exits non-zero
with the binary already installed and names both ways forward: run `orbit init` from the
intended account, or reinstall with `ORBIT_SKIP_HOST_PREREQUISITES=1` (below) in an
image build or container that runs as root.

Preparation first runs Orbit's exact namespace-and-mount probe as that account and checks
`--bind-fd` and `--ro-bind-fd`. A ready host causes no package or profile writes. When required, Orbit uses
an available package manager and, on Ubuntu with the exact
`setting up uid map: Permission denied` probe failure, only the packaged
`bwrap-userns-restrict` AppArmor rule. When the host still has no Bubblewrap, or only one
without either descriptor-backed bind option, Orbit installs its signed [bundled Bubblewrap](#bundled-bubblewrap). Administrator authentication is requested
only during explicit interactive installation/onboarding. `orbit init --non-interactive`
requires root or existing/passwordless `sudo` authority and never waits for a password.
Package/profile failures are retryable with `orbit init --host-prerequisites-only` after
correcting the reported cause. This explicit host-preparation command exits non-zero on
failure and does not seed an Orbit root or machine identity. Dispatch itself never
elevates, installs, reloads a profile, or falls back.

A failed preparation prints a warning with the reason and remedy; normal `orbit init`
continues to seed the Orbit root and exits zero if initialization succeeds. This applies
in interactive and non-interactive mode, including missing package managers, denied sudo
authentication, and kernel/container namespace denial. The warning names any privileged
package/profile commands attempted, because host changes may be partial. Declining or
failing sudo authentication stops preparation immediately without running further
privileged commands. `linux-bwrap` dispatch stays blocked until `orbit doctor providers`
reports the sandbox ready; successful init alone does not establish readiness.

On Linux, `orbit init --format json` (or `--json`) includes `linux_sandbox` with `status`
(`ready`, `skipped`, or `not_ready`) and `reason`. Warnings go to stderr, leaving stdout
as a JSON document. `skipped` means preparation was deliberately omitted, including for
a custom `--root`; it does not certify sandbox readiness.

Where an administrator or an image build owns the host's packages and security policy, pass
`--skip-host-prerequisites` (or set `ORBIT_SKIP_HOST_PREREQUISITES=1`): init then seeds
Orbit without touching the host, and `linux-bwrap` dispatch stays fail-closed until
`orbit doctor providers` reports the sandbox ready. The repository's `.cargo/config.toml`
sets that variable so test fixtures, which run `orbit init` under an isolated `HOME`, never
ask for `sudo` or change the machine running the tests.

`orbit doctor providers --json` reports each executor's configured `sandbox` separately
from `sandbox_ready` and `sandbox_readiness_detail` for `linux-bwrap`. Readiness is a fresh
capability check for the invoking user, not an inference from the executor setting. When a
Bubblewrap passed the check, `sandbox_wrapper` says which one (`host` or `bundled`), with
`sandbox_wrapper_path` and `sandbox_wrapper_version`; the table's `BWRAP` column shows the
same, for example `host 0.11.1` or `bundled 0.12.0`.

## Capability-based support matrix

Linux support depends on the capability probe, not a distribution or version allow-list:
Bubblewrap must provide `--bind-fd` and `--ro-bind-fd` and create the user namespaces and mounts Orbit needs
as the intended unprivileged account. A host that already passes needs no preparation.
An initial kernel/container namespace denial stops before package changes.

When Bubblewrap is missing or lacks either descriptor-backed bind option, Orbit selects an installed package
manager at its fixed `/usr/bin/` path. `ID` and the ordered `ID_LIKE` families in
`/etc/os-release` select the preferred manager when several are present. If none of those
is available, Orbit tries `apt-get`, `dnf5`/`dnf`, `pacman`, then `zypper`, in that order;
`dnf5` takes precedence over `dnf`. Versions do not affect selection. It installs
`bubblewrap`, then repeats the probe. If the package remains missing or lacks either option,
or no supported manager is installed, Orbit tries the signed bundled Bubblewrap.
Failure reports retain the missing capability and identify a missing manager or unavailable
bundled release. Namespace or other host-policy denial never triggers a bundled install.

These rows describe automatic preparation paths, covered by Host-boundary decision and
fault-injection tests. **No row has native package/profile/onboarding and sandboxed-subprocess
validation recorded here.** The native integration column makes that limit explicit;
package inventories alone do not establish readiness.

| Distribution/family (any version) | Preferred package path, when installed | Security-policy handling | Native integration |
|---|---|---|---|
| Ubuntu | `apt-get`: `bubblewrap` | Only the exact uid-map denial permits installing `apparmor-profiles` and loading packaged `bwrap-userns-restrict`; preserve custom or loaded profiles | Not run |
| Debian, Mint, Pop!_OS and other `ID_LIKE=ubuntu/debian` derivatives | `apt-get`: `bubblewrap` | Preserve existing host policy; Ubuntu's profile remedy applies only to `ID=ubuntu` | Not run |
| Fedora, RHEL, Rocky, AlmaLinux, CentOS and derivatives | `dnf5` or `dnf`: `bubblewrap` | Preserve existing host policy; require probe | Not run |
| Arch and derivatives | `pacman`: `bubblewrap` | Preserve existing host policy; require probe | Not run |
| openSUSE, SUSE and derivatives | `zypper`: `bubblewrap` | Preserve existing host policy; require probe | Not run |
| Other or unidentified distributions | Any installed supported manager, in the fallback order above | Preserve existing host policy; require probe | Not run |
| No supported package manager installed | Bundled Bubblewrap when the host binary is missing or lacks either descriptor-backed bind option | Preserve existing host policy; report the manager and capability gaps if the bundle cannot be installed | Not run |

Package availability references: [Ubuntu Noble bubblewrap](https://packages.ubuntu.com/noble/bubblewrap),
[Debian Trixie bubblewrap](https://packages.debian.org/trixie/bubblewrap),
[Fedora bubblewrap](https://packages.fedoraproject.org/pkgs/bubblewrap/bubblewrap/),
[Rocky 10 repository](https://download.rockylinux.org/pub/rocky/10.2/BaseOS/x86_64/os/Packages/b/),
and [Arch bubblewrap](https://archlinux.org/packages/extra/x86_64/bubblewrap/).
Package listings alone do not establish namespace policy or an unprivileged probe pass.

## Bundled Bubblewrap

Some supported hosts package a Bubblewrap older than 0.8.0, which lacks `--bind-fd`
(Ubuntu 22.04 ships 0.6.1), or none at all. Each Orbit release therefore publishes a
static Bubblewrap built from a pinned upstream release for x86_64 and aarch64. It is a
fallback only: the host's `/usr/bin/bwrap` is used whenever it advertises both descriptor-backed bind options, and
nothing is bundled onto such a host.

**When it is installed.** `orbit init` (and the shell installer, which runs it) installs
the bundled binary only when the probe reports that `/usr/bin/bwrap` is missing or lacks
either descriptor-backed bind option, after the distribution package path, if any, has been tried. Namespace
denial by the kernel or an enclosing container never triggers it. Ubuntu's AppArmor
remedy requires the exact uid-map denial on any version with the packaged profile;
derivatives and other distributions do not receive it. The bundled binary is never setuid; it needs the same
unprivileged user namespaces as the packaged one.

**Trust model.** The bundled binary lives at one fixed, root-owned path:
`/usr/local/libexec/orbit/bwrap`, mode `0755`, owned by `root:root`, under root-owned
directories that neither group nor others can write. Executors run only that path or
`/usr/bin/bwrap`, never a `PATH` lookup or any other location. Immediately before every
spawn Orbit re-checks the bundled path with `lstat`: a regular file (not a symlink), owned
by root, not group- or other-writable, not setuid or setgid, and executable, with every
ancestor directory a real directory owned by root and not group- or other-writable. A
sandboxed agent runs as the unprivileged invoking user, so it can neither rewrite the
binary nor rename anything over it, whatever paths its write policy binds. A user-owned
copy verified by digest before each spawn was rejected: the check and the `exec` would
race against the same user who owns the file.

**Verification before install.** `orbit init` fetches `orbit-checksums.txt` and its
signature from the release that matches the running Orbit version, verifies the signature
against the same release-signing trust set as `install.sh` and `orbit update`, and checks
`orbit-bwrap-<arch>-linux` against the signed digest. An unsigned manifest, a missing
entry or a digest mismatch stops before `sudo`. Only then does it run, with administrator
authority, `install -d -o root -g root -m 0755 /usr/local/libexec/orbit` and
`install -o root -g root -m 0755 <staged> /usr/local/libexec/orbit/bwrap`, and it re-hashes
the installed file, removing it if it differs from the signed digest. The shell installer
with a custom `ORBIT_INSTALL_BASE_URL` mirrors that release for this step;
`ORBIT_UPDATE_RELEASE_DIR` and `ORBIT_INSTALL_REPO` select the source as for `orbit update`.

**Updates.** When the bundled binary is installed, `orbit update` finishes by running the
new version's `orbit init --host-prerequisites-only --non-interactive`, which replaces a
bundled binary whose version differs from the new release's pin. Without passwordless
`sudo` that step reports a failure and its recovery command; the previous bundled binary
stays in place and keeps working while it still passes the probe. To remove the bundled
binary, delete `/usr/local/libexec/orbit/bwrap` as root; Orbit then uses the host's
Bubblewrap if it qualifies and otherwise reports the sandbox not ready.

**Licence and sources.** Bubblewrap is LGPL-2.0-or-later and the build links libcap
statically. Each release publishes, alongside the binaries and listed in the signed
`orbit-checksums.txt`, the exact upstream source tarballs (`bubblewrap-<version>.tar.xz`,
`libcap-<version>.tar.xz`), the build recipe `orbit-bwrap-build.sh`
([`scripts/build-bundled-bwrap.sh`](../../scripts/build-bundled-bwrap.sh)), a
`.buildinfo` per architecture recording the pinned inputs and toolchain, and
`orbit-bwrap-NOTICE.txt` with the licence texts. The release workflow builds them natively
on each architecture in a digest-pinned Alpine image. To bump Bubblewrap, change
`BWRAP_VERSION` and its SHA-256 in the script together with `BUNDLED_BWRAP_VERSION` in
`crates/orbit-exec/src/linux_sandbox/wrapper.rs`.

## Why the probe fails closed

Linux executors use `linux-bwrap`, which runs only `/usr/bin/bwrap` or the root-owned
bundled `/usr/local/libexec/orbit/bwrap` and fails closed when neither has the required
capabilities. Ubuntu can restrict unprivileged user
namespaces through AppArmor; the narrow packaged rule can grant Bubblewrap the needed
namespace access without changing the global restriction. Existing custom profile files
or loaded profiles that still fail the probe are preserved and reported as conflicts.
Kernel or enclosing-container denial is reported separately and is not repaired by
package installation. Do not disable the global user-namespace restriction, use a
setuid workaround, switch the executor off, or enable fallback to make a failed probe
look ready.

The Linux boundary enforces writes from the resolved policy. It leaves host filesystem
reads and host network access available, so it does not provide worktree-only reads or
policy-gated network egress. Well-known credential locations are masked inside the
sandbox. See [policy-sandbox](../design/policy-sandbox/) for the design.

## Verify the Bubblewrap boundary natively

After changing managed-worktree policy or the Linux spawn path, run the live
Bubblewrap suite from the candidate checkout on the owning Linux host:

```sh
cargo test -p orbit-exec --test sandbox linux_sandbox:: -- --include-ignored --nocapture
```

On Linux, the `linux_sandbox::` module in the `sandbox` integration-test target
includes live children through `spawn_under_linux_bwrap` and checks
credential masking, worktree writes, protected `.env` paths, Orbit store
exceptions and Git metadata integrity. Tests that cannot create the namespace
print `skipping real Bubblewrap test` and return early; the ignored ones fail
instead. Confirm nonempty selection without spawning children first:

```sh
cargo test -p orbit-exec --test sandbox linux_sandbox:: -- --list
```

The module is Linux-only: an empty list on another platform proves no Linux
boundary behavior. Record the command's actual exit status and mark native
enforcement **not run** when capability is unavailable. Fix the namespace
prerequisite and rerun on the owning host; a capability denial is never a passing skip.

The hosted `ci` workflow runs this suite in its Linux enforcement gate on an
ephemeral `ubuntu-latest` runner, which ships neither Bubblewrap nor the
AppArmor rule. The step before the gate installs the distribution `bubblewrap`
and `apparmor-profiles` packages and runs the same `/usr/bin/bwrap`
namespace-and-mount probe as the unprivileged runner user. Only the Ubuntu
`setting up uid map: Permission denied` failure triggers loading the packaged
`bwrap-userns-restrict` profile; any other probe failure, an existing bwrap
profile, or an existing `/etc/apparmor.d/bwrap-userns-restrict` fails the step
with diagnostics. It never changes the global user-namespace controls.

Step-failure recovery dispatch through Bubblewrap with a provider process has
no automated native check. Its persistent Git configuration guard runs in the
ordinary `orbit-core` suite.

A `bwrap: No permissions to create new namespace` failure inside an
agent-executor or job-run worktree is nested-sandbox environment, not a missing
AppArmor profile. The outer containment blocks nested `unshare(CLONE_NEWUSER)`
even when `/proc/sys/kernel/unprivileged_userns_clone` is `1` and `unshare -U`
succeeds; that is distinct from the host UID-map error this runbook remediates.
Do not disable `linux-bwrap` or try to make bwrap nest. Live bwrap spawn checks
(`spawn_under_linux_bwrap` and the ignored live-spawn tests above) belong on
the owning Linux host: replay them there with `--run-ignored` or operator
replay, and record a nested denial as **not run**.

## If the probe still fails

Read `sandbox_readiness_detail` from `orbit doctor providers --json`. Missing
privileges, an incompatible `--bind-fd` feature, a custom-profile conflict,
and kernel/container denial have different remedies. Correct the reported
cause, then run `orbit init --host-prerequisites-only` and recheck doctor. Do not disable
`kernel.apparmor_restrict_unprivileged_userns` globally or enable
`allow_fallback`; both weaken or bypass the fail-closed boundary.

Other distributions need the same two conditions: a Bubblewrap with `--bind-fd` (the
host's `/usr/bin/bwrap` or the bundled one) and permission for an unprivileged user to
create user namespaces and mounts. Non-Linux hosts are unaffected: macOS uses
`sandbox-exec`, and platforms without a shipped OS-level backend rely on in-process
filesystem guards only.

## Explicitly disable worker sandboxing

An operator can persistently disable sandboxing for an executor by setting
`spec.sandbox: off` in its host-global resource, normally
`~/.orbit/resources/executors/codex.yaml` (use the corresponding filename for
another provider).

Before editing shared executor YAML, complete the mixed-version rollout below.
Replacing the CLI binary does not update an already-running MCP server or job
runner. Preserve the rest of the resource when changing the sandbox field:

```yaml
spec:
  # Keep the executor's existing command, arguments, and other settings.
  sandbox: off
```

This setting applies to future worker launches using that executor across
workspaces on the host. It survives fresh runtime opens, ordinary `orbit init`
(without `--force`), repeated default seeding, and normal `orbit workspace
sync`. `orbit init --force` resets the global root to shipped defaults,
including executor sandbox, and therefore restores the sandboxed shipped
value. It also discards the machine identity and creates a new machine ID.
Non-interactive resets require both `--machine-name` and `--task-prefix`;
missing non-interactive inputs or invalid supplied identity flags are refused
before deleting the root.
Existing processes keep their launch configuration.

`off` is distinct from an omitted or `null` sandbox field. Omitted/null values
on installed Linux defaults are legacy unspecified settings and migrate to
`linux-bwrap`; deleting the field is therefore not a persistent opt-out.
Concrete backend names keep their platform validation. Invalid spellings such
as `disabled` or boolean `false` fail resource parsing.

With `off`, Orbit does not probe or launch Bubblewrap or `sandbox-exec`, and it
neutralizes the supported provider-inner sandbox flags (including Codex's
`--sandbox danger-full-access`). The workspace `execution.codex.sandbox` key
alone controls Codex's inner mode, not Orbit's outer wrapper. Explicit executor
`off` takes precedence over that inner mode. Tool authorization and policy
checks still apply to Orbit tool calls; provider subprocess filesystem access
has no Orbit sandbox confinement.

Inspect the configured choice with `orbit doctor providers --json` (the `codex`
entry's `sandbox` field is `"off"`). The invocation audit reports `sandbox_backend: off`,
no trusted wrapper or probe, and `write_unrestricted` / `read_unrestricted`.
This is an operator opt-out, not a fallback after a failed security check, and
successful bare execution does not establish Bubblewrap or AppArmor enforcement.

### Mixed-version rollout and rollback

Older Orbit readers accept only the concrete backend enum values. They reject
`off` with `spec.sandbox: unknown variant off`; retaining `schemaVersion: 2`
does not make this new value readable by those binaries. New Orbit versions
continue to read existing resources, but backward reading of `off` by an old
version is unsupported. Executor loading during bootstrap/seeding or dispatch
can therefore break ordinary operations before they reach their requested tool.

Use this order on every process sharing the host resource directory:

1. Keep the existing executor YAML unchanged while installing the updated
   executable. Pause new dispatch and let old workers and active drain/job
   supervisors finish, or stop them through their normal lifecycle. Keep a
   backup of the pre-change executor files.
2. Restart persistent `orbit mcp serve` processes and reconnect their clients.
   Also restart long-lived drain/workflow runners, orchestration processes,
   and web/dashboard servers that open runtimes or dispatch work. Any still-active
   sweep process or worker that can load executors or start nested Orbit commands
   must finish or restart from the updated executable. Check service/timer
   launch paths and worker `ORBIT_BIN` overrides for older executable copies.
3. Verify each restarted reader's executable provenance and run an ordinary
   read-only task/tool call through each authoritative MCP connection while the
   YAML still uses the old values. A successful new CLI `--version` invocation
   alone does not verify the executable already loaded by another process.
4. Only after all readers have been replaced, set `spec.sandbox: off`. Verify
   the provider's `sandbox` in `orbit doctor providers --json`, repeat an
   ordinary authoritative MCP read, and inspect the next invocation's effective sandbox audit before
   resuming normal dispatch. If an older reader must remain active, defer the
   shared YAML change; deleting the sandbox field is not a compatibility solution.

For rollback to an older binary, pause dispatch and restore the backed-up
concrete sandbox values **before** starting any old reader. Then restart the
affected services/connections and verify ordinary reads again. Do not launch
an old MCP server or drain against shared executor files that still contain
`off`. These steps change Orbit processes and resources, not host AppArmor
settings.

### Runtime grant race regression

Runtime directory and SQLite sidecar grants are passed to Bubblewrap as held
file descriptors using `--bind-fd`; the capability probe rejects older builds
that do not advertise this option. Run the explicit kernel regression on an
authorized Linux host after building the candidate revision:

```sh
cargo test -p orbit-exec --test sandbox \
  linux_sandbox::kernel_descriptor_mount_never_writes_the_replacement_object -- --ignored --exact
```

The test deliberately replaces a regular sidecar name after its descriptor is
opened and proves that the replacement is unchanged. Namespace denial is a
test failure, not a skip. Ordinary tests also cover descriptor closure and
fail-closed external-symlink replacement without requiring user namespaces.
The contract assumes concurrent writers cannot perform privileged remounts of
the already-open runtime root or its ancestors. macOS uses its existing
Seatbelt path rules; other platforms reject `linux-bwrap` at dispatch.
