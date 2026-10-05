---
type: runbook
summary: Run Orbit on Windows inside a WSL2 Linux distribution, with sandbox, MCP, clock and filesystem limits and an explicit not-verified-on-Windows matrix.
tags: [operations, windows, wsl2, onboarding, sandbox, mcp, scheduler]
paths:
  - "install.sh"
  - "npm/**"
  - "crates/orbit-cli/src/command/init/**"
  - "crates/orbit-cli/src/command/mcp/**"
  - "crates/orbit-cli/src/command/clock/**"
  - "crates/orbit-core/src/application/routines/clock/**"
  - "crates/orbit-exec/src/**"
related_features: [policy-sandbox, executors, routines]
related_artifacts: []
last_validated: 2026-10-05
---

# Run Orbit on Windows through WSL2

Use this runbook to set up Orbit on a Windows machine. Orbit has no native
Windows build: the supported route is the Linux build running inside a WSL2
Linux distribution, where the [Linux sandbox](linux-sandbox.md), clock and
setup paths apply unchanged. Native Windows is **compile-checked only**
([`ci-windows.yml`](../../.github/workflows/ci-windows.yml) runs `cargo check`
for `x86_64-pc-windows-msvc`; no test runs on Windows).

> **NOT VERIFIED ON WINDOWS.** Nobody has executed this procedure on a Windows
> host. Orbit commands below are checked against the Linux CLI help and source
> of the version in the [verification matrix](#verification-matrix); the
> Windows-side commands come from Microsoft's documentation. Treat every step
> that depends on WSL behaviour — the Bubblewrap probe, systemd user units,
> Windows MCP clients launching `wsl.exe`, and background lifetime — as
> unproven until its check passes on your machine. Neither Linux test success
> nor the Windows compile check proves it.

## 1. Safety and scope

- Everything Orbit runs (CLI, provider CLIs, `gh`, Git, worktrees, `~/.orbit/`
  and `<repo>/.orbit/`) lives **inside one distribution, as one Linux user**.
  Windows only hosts that distribution and, optionally, launches Orbit's MCP
  server through `wsl.exe`.
- Orbit's sandbox fails closed. If the Bubblewrap probe fails inside WSL,
  agent dispatch stays blocked. Do not set an executor's `spec.sandbox: off`,
  enable `allow_fallback`, install a setuid `bwrap`, or loosen the
  distribution's user-namespace policy to get past it; report the probe detail
  instead (see [section 5](#5-prepare-and-check-the-sandbox)).
- Installing WSL needs Windows administrator rights and usually a reboot.
  Changing `/etc/wsl.conf` or `%UserProfile%\.wslconfig` needs a distribution
  restart, which stops every Orbit process inside it.

## 2. Install WSL2 and a distribution

Microsoft's documentation, checked 2026-10-05, requires Windows 10 version 2004
(build 19041) or later, or Windows 11
([Install WSL](https://learn.microsoft.com/en-us/windows/wsl/install)).
From an **administrator** PowerShell:

```powershell
wsl --list --online          # distributions available to install
wsl --install -d <Distro>    # e.g. Ubuntu-24.04; plain `wsl --install` installs Ubuntu
```

Restart Windows when asked, open the distribution once, and create its Linux
user. New installs default to WSL 2; check and, if needed, convert:

```powershell
wsl --list --verbose                 # VERSION column must read 2
wsl --set-version <Distro> 2
wsl --version                        # WSL 0.67.6 or newer is needed for systemd
wsl --update                         # if --version is rejected or too old
```

`<Distro>` is the exact name `wsl --list --verbose` prints; you will reuse it
for the MCP and clock steps. Prefer a distribution with an automatic
preparation path in the
[Linux sandbox support matrix](linux-sandbox.md#capability-based-support-matrix)
(Ubuntu, the WSL default, is one); that matrix records no native run for any
distribution either. Command reference:
[Basic commands for WSL](https://learn.microsoft.com/en-us/windows/wsl/basic-commands).

## 3. Enable systemd in the distribution

Orbit's clock is a systemd **user** timer and its worker containment uses
transient systemd user scopes, so the distribution should boot systemd.
Microsoft documents systemd as the default for the current Ubuntu installed by
`wsl --install`; other distributions need it enabled
([Use systemd with WSL](https://learn.microsoft.com/en-us/windows/wsl/systemd)).
Inside the distribution, make sure `/etc/wsl.conf` contains:

```ini
[boot]
systemd=true
```

Then restart the distribution from PowerShell and confirm from inside it:

```powershell
wsl --shutdown               # stops ALL running distributions and their processes
```

```bash
systemctl status | head -n 3          # "State: running" (or "degraded") means systemd is PID 1
systemctl --user status | head -n 3   # your user manager must answer for the clock and scopes
```

Microsoft notes that on Debian/Ubuntu/Kali the `systemd` and `systemd-sysv`
packages must be installed. Without a working user manager, workers run in the
caller's cgroup with one warning (`machine.worker_containment`; refused when
`machine.worker_containment_strict` is on — see [CONFIG.md](../CONFIG.md)) and
`orbit routine init --install-clock` cannot install the timer.

## 4. Install the toolchain, Orbit and provider CLIs inside the distribution

Run everything from here on **inside the distribution, as the Linux user that
will run Orbit** — not through `sudo`, and not from PowerShell.

```bash
sudo apt-get update && sudo apt-get install -y git curl ca-certificates   # Debian/Ubuntu; use your distro's manager otherwise
```

Install Orbit with one method, exactly as on Linux (see the
[README quick start](../../README.md#quick-start)):

```bash
npm install -g @orbit-tools/cli     # needs Node 18+ installed inside the distro
# or the signed shell installer, which installs to ~/.orbit/bin/orbit:
curl -sSf https://raw.githubusercontent.com/constellation-works/orbit/main/install.sh | sh
```

Install and sign in to at least one agent CLI **inside the distribution**
(follow that provider's Linux instructions), plus `gh auth login` if you want
pull requests. A provider signed in on Windows is a different installation
with different credentials; Orbit cannot use it.

WSL appends the Windows `PATH` to Linux `PATH` by default, so a Windows npm
shim can shadow a missing Linux CLI. Check that every tool resolves inside the
Linux filesystem:

```bash
command -v orbit gh git <provider-cli>    # none of these may start with /mnt/
```

If one does, install the Linux build. To stop the Windows `PATH` leaking in,
set `appendWindowsPath=false` under `[interop]` in `/etc/wsl.conf` and restart
the distribution ([WSL configuration](https://learn.microsoft.com/en-us/windows/wsl/wsl-config#interop-settings)).

## 5. Initialize the machine and prepare the sandbox

```bash
orbit init                           # interactive: machine name and task prefix
orbit init --non-interactive --machine-name <name> --task-prefix <PREFIX>
orbit init --format json             # includes linux_sandbox.status and reason
```

`orbit init` does here what it does on any Linux host (full detail in the
[Linux sandbox runbook](linux-sandbox.md)): it runs Orbit's namespace-and-mount
probe as the invoking user and checks `--bind-fd`. A ready host changes
nothing. Otherwise it installs the distribution's `bubblewrap` package, loads
Ubuntu's packaged `bwrap-userns-restrict` AppArmor rule only for the exact
`setting up uid map: Permission denied` failure, and, when the host
Bubblewrap is still missing or lacks `--bind-fd`, installs Orbit's signed
bundled Bubblewrap at `/usr/local/libexec/orbit/bwrap` (root-owned, never
setuid). Executors run only `/usr/bin/bwrap` or that bundled path. A
kernel or container namespace denial is reported, never "repaired".

Check readiness — successful `orbit init` alone does not establish it:

```bash
orbit doctor providers --json        # per executor: sandbox, sandbox_ready, sandbox_readiness_detail,
                                     # sandbox_wrapper (host|bundled), sandbox_wrapper_path, sandbox_wrapper_version
```

What must work inside WSL: `sandbox_ready` is `true` for the executors you
will dispatch. If it is `false`, read `sandbox_readiness_detail`, fix the
named cause, then retry with `orbit init --host-prerequisites-only` and
recheck. **NOT VERIFIED ON WINDOWS:** whether the WSL2 kernel and your
distribution let an unprivileged user create the namespaces and mounts the
probe needs. If they do not, dispatch stays blocked; that is the designed
outcome, not a reason to disable the sandbox.

## 6. Keep repositories and state on the Linux filesystem

Clone repositories under the distribution's home, for example
`~/src/<repo>`, not under `/mnt/c/...`. Microsoft recommends storing files used
from a Linux command line in the WSL filesystem for performance
([Working across file systems](https://learn.microsoft.com/en-us/windows/wsl/filesystems)).
For Orbit it also matters for correctness:

- `~/.orbit/` and `<repo>/.orbit/` hold SQLite databases in WAL mode and
  per-run worktrees. Windows drives are mounted through DrvFs, where Linux
  permission bits are not stored unless the `metadata` mount option is on and
  names are case-insensitive by default; Orbit's root-owned and
  owner-only permission checks and its file locks assume a Linux filesystem.
- Do not point `orbit --root`, `ORBIT_INSTALL_DIR` or a workspace at
  `/mnt/<drive>/...`, and do not edit `.orbit/` from Windows tools through
  `\\wsl$\...` while Orbit is running.
- Windows editors can open the checkout through `\\wsl.localhost\<Distro>\home\<user>\...`
  (or `\\wsl$\...`). Use Linux Git inside the distribution for commits; a
  Windows Git on the same checkout sees different line-ending and permission
  settings.

## 7. Initialize the workspace

From the repository root inside the distribution:

```bash
cd ~/src/<repo>
orbit workspace init --base-branch <integration-branch> --ship-mode pr --mcp
orbit workspace show --format json
orbit doctor
```

`--mcp` writes MCP registrations for agent clients **installed inside the
distribution**, under the Linux home or the checkout (for example
`~/.claude.json`, `~/.codex/config.toml`, or repo-local files), with operator
authority. Neither it nor `orbit mcp init` edits any Windows client's
configuration under `%UserProfile%` or `%AppData%`. Omit `--mcp` if you will
only use Windows-side clients (next section).

## 8. Connect a Windows-side MCP client

A Windows desktop client can launch the Linux server through `wsl.exe`. Find
the real Linux executable first; a bare `orbit` relies on a login shell's
`PATH`, which `wsl.exe --exec` does not run:

```bash
command -v orbit                                   # ~/.orbit/bin/orbit for the shell installer
echo "$(npm root -g)/@orbit-tools/cli/binaries/orbit"   # native binary behind the npm shim
orbit workspace show --format json                 # workspace.id is the ws_* selector
whoami
```

For an npm install, use the native binary under `binaries/` rather than
`bin/orbit.js`: the shim needs `node` on `PATH`, which a non-login
`wsl.exe --exec` launch may not have. Then add a stdio server to the Windows
client's own MCP configuration (file location and schema are the client's).
For a JSON-configured client the entry looks like:

```json
{
  "mcpServers": {
    "orbit": {
      "command": "wsl.exe",
      "args": [
        "--distribution", "<Distro>",
        "--user", "<linux-user>",
        "--exec", "/home/<linux-user>/.orbit/bin/orbit",
        "mcp", "serve", "--workspace", "<ws_id>"
      ]
    }
  }
}
```

- `--distribution` and `--user` pin the intended distribution and Linux user
  instead of whichever is the WSL default. `--exec` runs the binary without a
  shell, so the arguments reach Orbit unchanged and nothing a shell profile
  prints corrupts the JSON-RPC stream on stdout.
- Use an absolute Linux path (`/home/...`), not a Windows path, and keep Orbit
  options after the executable path.
- This entry is **agent** authority, the same as `orbit mcp init`. Add
  `"--operator"` after `"serve"` only for a deliberate, human-facing
  orchestrator that may dispatch workflows, resume runs and run governed
  commands — the authority `workspace init --mcp` installs on Linux. Never give
  `--operator` to a server an agent launches.
- `--workspace` binds the session to one registered workspace; it is resolved
  against the Linux registry, so the Windows working directory does not
  matter.
- The server runs in a minimal non-login environment. Verify, through the
  same launch shape, that Orbit and the providers it may start resolve:

  ```powershell
  wsl.exe --distribution <Distro> --user <linux-user> --exec /home/<linux-user>/.orbit/bin/orbit doctor providers --json
  ```

**NOT VERIFIED ON WINDOWS:** no Windows MCP client has been exercised with
this entry. Prove the connection by discovering the workspace and running one
read-only audited call (for example `orbit.task.list`) from that client; a
version string is not proof. For the shape of the remote and federated modes,
see the [remote access reference](../../crates/orbit-core/assets/skills/orbit-setup/references/remote-access.md).

## 9. Scheduler: the clock inside WSL

The clock is optional; task tracking and on-demand runs do not need it. With
systemd running (section 3):

```bash
orbit routine init --install-clock   # installs ~/.config/systemd/user/orbit-sweep.{service,timer}
orbit clock status                   # cadence and native manager state
orbit clock pause                    # stop scheduled ticks; manual `orbit clock tick` still works
orbit clock enable                   # re-arm at the configured cadence
orbit clock repair                   # rewrite a unit naming a missing, moved or stale orbit binary
orbit clock set --cadence-seconds 300
```

Routines and auto-tasks still ship disabled; enabling them follows the
[automation reference](../../crates/orbit-core/assets/skills/orbit-setup/references/automation.md).
The service's `ExecStart` names the Orbit binary that installed it, so rerun
`orbit clock repair` after moving or reinstalling Orbit.

The scheduler only runs while the distribution runs. Microsoft states that
systemd services do **not** keep a WSL instance alive. Expect no ticks while:

- the distribution or WSL VM is stopped (`wsl --shutdown`,
  `wsl --terminate <Distro>`, Windows restart or sign-out);
- Windows is asleep or hibernating;
- WSL has shut an idle instance down — `.wslconfig` documents
  `[general] instanceIdleTimeout` and, on Windows 11, `[wsl2] vmIdleTimeout`
  ([WSL configuration](https://learn.microsoft.com/en-us/windows/wsl/wsl-config)).

Each routine's `missed_run` policy (`skip` or `catch_up_once`) decides what the
next tick does with slots missed while the distribution was down. Do not rely
on WSL for an always-on scheduler or long unattended drains; use a Linux or
macOS host for that. **NOT VERIFIED ON WINDOWS:** the systemd user timer,
`orbit clock status` against WSL's user manager, and how long detached Orbit
workers survive once no terminal is open.

## 10. Verify

Inside the distribution:

```bash
orbit --version
orbit workspace show --format json
orbit doctor
orbit doctor providers --json        # sandbox_ready must be true for executors you dispatch
orbit clock status                   # only if you installed the clock
```

Then exercise the workflow you set up: a read-only MCP call from the client
you configured, and, only when agent execution is wanted and authorized, one
disposable task through the normal pipeline. Record any WSL-specific failure
with its exact command and output.

## Verification matrix

Recorded 2026-10-05 against Orbit 0.27.0 at source commit `fa0b727d9`
(`agent-main`). "Help/source" means checked against the CLI help or source of
that commit on Linux; it does not mean the command ran under WSL.

| Step | Help/source checked | Executed on Linux | Executed on Windows/WSL |
|---|---|---|---|
| `wsl --install`, `--list`, `--set-version`, `--version`, `--update`, `--shutdown`, `--terminate` | Microsoft docs, 2026-10-05 | n/a | **Not verified** |
| `wsl.exe --distribution/--user/--exec` argument handling | `wsl.exe` usage text (Microsoft WSL repository), 2026-10-05 | n/a | **Not verified** |
| `/etc/wsl.conf` `[boot] systemd=true`, `[interop] appendWindowsPath` | Microsoft docs, 2026-10-05 | n/a | **Not verified** |
| `orbit init` flags, `--format json` `linux_sandbox` | CLI help and init source | Linux sandbox runbook covers the Linux path | **Not verified** |
| Bubblewrap probe, bundled `bwrap`, AppArmor rule | `crates/orbit-exec`, init source, [Linux sandbox runbook](linux-sandbox.md) | No native onboarding run is recorded for any distribution | **Not verified** |
| `orbit doctor providers --json` readiness fields | CLI help and doctor source | Ran read-only on a Linux host: a JSON array with the fields above; inside an agent sandbox the probe reported the nested-namespace denial, as expected | **Not verified** |
| `orbit workspace init --mcp`, `orbit mcp init` write Linux-side files only | `orbit-cli` MCP setup source | Not run for this runbook | n/a (does not touch Windows) |
| `orbit mcp serve --workspace/--operator` | CLI help and source | Not run for this runbook | **Not verified** through `wsl.exe` |
| `orbit routine init --install-clock`, `orbit clock status/pause/enable/repair/set` | CLI help and clock source | Not run for this runbook | **Not verified** |
| `orbit workspace show --format json`, `orbit doctor` | CLI help | `workspace show --format json` ran read-only; `workspace.id` carries the selector. `orbit doctor` not run | **Not verified** |

## Related references

- [Linux sandbox onboarding and diagnostics](linux-sandbox.md)
- [Configuration reference](../CONFIG.md) — `machine.worker_containment`
- [Setup skill: Windows through WSL2](../../crates/orbit-core/assets/skills/orbit-setup/references/windows-wsl2.md)
- [Windows compile check](../DEVELOPMENT.md#windows-compile-check)
