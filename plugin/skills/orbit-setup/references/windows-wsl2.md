# Windows through WSL2

Use this when the user's machine runs Windows. Orbit has no native Windows
build; it runs as the Linux build inside one WSL2 distribution, as one Linux
user, and every other reference in this skill applies inside that
distribution. Native Windows is compile-checked only. The full procedure,
Microsoft sources and verification matrix are in the
[Windows WSL2 runbook](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/windows-wsl2.md).

**NOT VERIFIED ON WINDOWS.** No one has run this path on a Windows host. Orbit
commands are checked against the Linux CLI help and source; Windows steps come
from Microsoft's documentation. Say so when you report, and prove each step on
the user's machine with the checks below rather than assuming it works.

## Establish the target

Ask, or detect from inside the distribution, which distribution and Linux user
Orbit will use:

```bash
grep -qi microsoft /proc/sys/kernel/osrelease && echo WSL   # running under WSL?
cat /etc/os-release; whoami
systemctl --user status | head -n 3                          # user manager available?
```

From Windows, `wsl --list --verbose` names the distributions; VERSION must be
2. Installing WSL, a distribution, or changing `/etc/wsl.conf` needs the user's
Windows administrator action and a distribution restart (`wsl --shutdown`
stops every distribution). These are separate choices; do not perform them
unasked.

## Inside the distribution

Follow [first-run.md](first-run.md) as on Linux, with these WSL rules:

1. **systemd.** The clock and worker containment need a systemd user manager.
   Current Ubuntu on WSL boots systemd; other distributions need
   `[boot]` `systemd=true` in `/etc/wsl.conf` and WSL 0.67.6 or newer.
2. **Linux tools only.** Install Orbit (npm or `install.sh`), Git, `gh` and each
   provider CLI inside the distribution and sign them in there. Windows `PATH`
   is appended by default: `command -v orbit gh git <provider>` must not
   resolve under `/mnt/`.
3. **Linux filesystem.** Keep repositories, `~/.orbit/` and `.orbit/` under the
   Linux home, never under `/mnt/c/...`. DrvFs drops Linux permission bits
   unless mounted with `metadata`, and Orbit's SQLite stores, worktrees and
   ownership checks assume a Linux filesystem.
4. **Sandbox.** Run `orbit init` as the Linux user, then require `sandbox_ready`
   from `orbit doctor providers --json`. Preparation and fail-closed behaviour
   are the same as on Linux: [linux-sandbox.md](linux-sandbox.md). Whether the
   WSL kernel admits the probe is unverified; a failed probe is a blocker to
   report, never a reason to set `spec.sandbox: off`, enable fallback or
   loosen namespace policy.

## Windows-side MCP clients

`orbit workspace init --mcp` and `orbit mcp init` write only Linux-side client
configuration (the Linux home or the checkout). A Windows client needs its own
entry, edited in that client's configuration, launching the absolute Linux
binary through `wsl.exe` without a shell:

```text
command: wsl.exe
args:    --distribution <Distro> --user <linux-user> --exec <absolute-linux-orbit>
         mcp serve --workspace <ws_id>
```

- `<absolute-linux-orbit>`: `~/.orbit/bin/orbit` expanded for `install.sh`, or
  `$(npm root -g)/@orbit-tools/cli/binaries/orbit` for npm (the `bin/orbit.js`
  shim needs `node` on a `PATH` that `--exec` does not set up).
- `<ws_id>`: `workspace.id` from `orbit workspace show --format json`.
- That entry is agent authority. Add `--operator` after `serve` only when the
  user deliberately wants a human-facing orchestrator client with dispatch
  authority; never for a server an agent launches. See
  [remote-access.md](remote-access.md#authority-on-the-destination).
- Verify through the client: discover the workspace and make one read-only
  audited call. Also run `orbit doctor providers --json` through the same
  `wsl.exe --exec` shape to see the environment that server gets.

## Scheduler limits

`orbit routine init --install-clock` installs the systemd user timer, managed
with `orbit clock status|pause|enable|repair` ([automation.md](automation.md)).
It ticks only while the distribution runs: systemd services do not keep WSL
alive, and `wsl --shutdown`, Windows sleep, restart, sign-out and WSL idle
shutdown stop it. Each routine's `missed_run` policy handles missed slots.
Present WSL as a workstation setup, not an always-on scheduler or unattended
drain host.

## Report

Distinguish what you executed on the user's machine from what is only
documented. Mark the sandbox probe, systemd user timer, Windows MCP client and
background lifetime **NOT VERIFIED ON WINDOWS** until their checks pass there.
