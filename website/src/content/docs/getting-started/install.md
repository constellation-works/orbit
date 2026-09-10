---
title: Install Orbit
description: "Install the Orbit CLI, initialize global and workspace state, prepare the sandbox, and stay current with orbit update."
sidebar:
  order: 2
---

## Install

The recommended install uses the npm binary proxy:

```bash
npm install -g @orbit-tools/cli
```

Node 18 or newer is required. The package downloads the matching native Orbit
binary and puts `orbit` on your `PATH`. Confirm it:

```bash
orbit --version
```

### Alternatives

From a trusted source checkout, the release installer detects the platform,
downloads the matching release binary, authenticates signed checksums, validates
the archive contents, and installs into `~/.orbit/bin`:

```bash
./install.sh
```

For development from a source checkout (this one needs the Rust toolchain):

```bash
make install
```

### Pinned versions and custom install directory

```bash
ORBIT_VERSION=vX.Y.Z ./install.sh
ORBIT_INSTALL_DIR="$HOME/.local/bin" ./install.sh
```

Replace `vX.Y.Z` with the release you intend to pin. Use the unpinned install
command above when you want the latest published release.

`ORBIT_VERSION`, `ORBIT_INSTALL_REPO`, and `ORBIT_INSTALL_BASE_URL` change the
release source the installer trusts, so use them only for pinned releases,
forks, or controlled test mirrors. `ORBIT_INSTALL_BASE_URL` may use any
downloader-supported scheme, including `file://` for tests; the signature check
protects artifact integrity, not transport confidentiality.

`ORBIT_RELEASE_TRUSTED_KEYS_FILE` is the preferred override for the full
trusted signing-key set, including key IDs, `not_after`, and `revoked_at`; it
requires `ORBIT_RELEASE_TRUSTED_KEYS_FILE_ACKNOWLEDGE_TRUST_CHANGE=1` and should
be limited to tests or emergency operations.
`ORBIT_RELEASE_PUBLIC_KEY_FILE` is **deprecated** in favor of the trusted-keys
file (which is a strict superset); it still works for the single-key case and
requires `ORBIT_RELEASE_PUBLIC_KEY_FILE_ACKNOWLEDGE_TRUST_CHANGE=1`. Setting
both files at once is refused.

## Initialize state

Orbit keeps global state under `~/.orbit/` and per-repository state under
`.orbit/` in each workspace.

```bash
orbit init
cd <repo>
orbit workspace init
```

`orbit init` asks for two things it can never change later on this machine: a
host name, and a **task-id prefix** of 2–5 uppercase letters that namespaces
every task ID this machine allocates. Supply them up front for an unattended
setup:

```bash
orbit init --non-interactive --host-name build-01 --task-prefix ORB
```

It also seeds `~/.orbit/config.toml` with crews for the provider CLIs it
detects, and installs the default skills under `~/.orbit/skills`, linking them
into `~/.agents/skills` and `~/.claude/skills`. The same reconcile removes
dangling Orbit-owned links for retired skill IDs and leaves your own custom
skills in place.

`orbit workspace init` registers the repository. Useful options:

```bash
orbit workspace init --ship-mode local      # deliver in place instead of opening PRs
orbit workspace init --base-branch develop  # default base for ship workflows
orbit workspace init --mcp                  # also set up MCP client integrations
orbit workspace init --inject-agent-rules   # add an Orbit rules block to CLAUDE.md / AGENTS.md
```

`--mcp` registers the Orbit MCP server with **operator** authority, which is what
lets an agent dispatch workflows and run governed operations. Plain
`orbit mcp init` registers the agent-only surface instead. See
[Set Up MCP](../../how-to/mcp-integration/) for the full picture.

## Prerequisites

You need at least one authenticated provider CLI, because agent activities
dispatch through it. `orbit init` probes `PATH` for `claude`, `codex`, `agy`
(Antigravity), `gemini`, `grok`, `copilot`, `cursor-agent`, `pi`, and
`opencode`, and seeds crews for the ones it finds. The
[setup explorer](../../concepts/agents/#set-up-an-executor) shows each
executor's binary and a starter crew. PR mode additionally needs the GitHub CLI
(`gh`) authenticated in the environment where Orbit runs.

Orbit itself installs without Rust. You only need a Rust toolchain to build from
source or contribute to the workspace.

## Prepare the sandbox

Orbit wraps each spawned agent subprocess in an OS-level sandbox scoped by the
activity's resolved filesystem profile. `orbit init` persists the
host-appropriate backend into the shipped executor assets:

| Platform | Backend | Behavior |
|---|---|---|
| macOS | `sandbox-exec` | The profile is compiled to SBPL and applied to the subprocess. |
| Linux | `bwrap` (Bubblewrap) | Writes are confined to the resolved profile. Host filesystem reads and host network access remain available. |
| Windows and others | none | No OS-level wrapper. Orbit's process supervision, tool allowlists, and in-process guards for its own built-in tools still apply. |

**Linux fails closed.** Dispatch requires a trusted `/usr/bin/bwrap` that passes
a namespace-and-mount capability probe. If Bubblewrap is missing, or the probe
fails, the run fails rather than executing unconfined — unless an executor
explicitly sets `allow_fallback: true`.

On Ubuntu 24.04 and other distributions that restrict unprivileged user
namespaces under AppArmor, install Bubblewrap and load the narrow profile before
your first dispatch:

```bash
sudo apt-get install --yes bubblewrap apparmor-profiles
test -x /usr/bin/bwrap
```

A run failing with `bwrap: setting up uid map: Permission denied` is this
prerequisite, not your task. The full procedure, including loading the
`bwrap-userns-restrict` AppArmor profile, is in the
[Linux sandbox runbook](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/linux-sandbox.md).

## Configure Orbit

`orbit init` writes a working `~/.orbit/config.toml`. Review the crews it
detected and pick a default:

```bash
orbit config show
orbit config keys
orbit config set workflow.default_crew opus
```

See [Configuration](../../reference/config/) for the file locations, the full
settable key list, and how crews resolve.

## Check the workspace

```bash
orbit doctor
```

`orbit doctor` diagnoses config, database, disk, index, lock, and run health.
It reports problems by default; the `--fix-*` flags are the opt-in repairs.

## Stay current

`orbit update` installs a published release and then converges this machine to
it:

```bash
orbit update --check          # report what is available, change nothing
orbit update                  # install the newest published release
orbit update --version 0.19.0 # install one exact release
```

The download is verified against the signed release checksum manifest before
anything is replaced. After the executable is swapped, the new binary applies
pending `.orbit` layout and store migrations and then reconciles managed
workspace assets, in that order. Re-running `orbit update` is idempotent and is
the supported way to finish a run that did not complete.

Two limits are worth knowing:

- Only installations made by Orbit's own installer can be replaced in place.
  Where a package manager owns the binary, `orbit update` reports the command
  that upgrades it instead of overwriting it.
- Installing an older release requires `--allow-downgrade`, and is permitted
  only if that release can still open this workspace's state.

To inspect migrations without applying them, use `orbit migrate` on its own;
`orbit migrate --confirm` applies them.
