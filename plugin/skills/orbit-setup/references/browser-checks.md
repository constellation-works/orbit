# Browser kit for UI checks

Use this on a host whose workers change web pages (a dashboard, a docs site,
any UI) and must show the rendered result. Orbit gives agents no interactive
browser. A worker checks UI by scripting headless Chromium through Playwright:
it serves the page on loopback, drives and asserts it, and saves screenshots
under its worktree's `.orbit/tmp/`, which it can then open and look at.

`.orbit/tmp/` is cleared with the worktree, so a worker whose diff changes
something a user sees attaches its screenshots as task artifacts with
`orbit.task.artifact.put`: each changed view in every theme it supports, at a
375px phone width when the layout is responsive, at a stable path such as
`screenshots/<view>-<theme>-<width>.png`, each under the 1 MiB artifact limit,
and listed in the handoff. A worker on a host without a verified kit says so
in its handoff instead of claiming visual verification. The orbit skill's
task-execution reference (Screenshots of a UI change) holds the full rule.

`orbit browser-kit` builds that kit once per host. Run it as the user who runs
workers, on the host, never inside a worker: on Linux Chromium's system
libraries can need root, which the sandbox forbids (`no new privileges`), and
a per-run download is slow and puts each run on a different browser build.
Node and npm must already be on PATH. Chromium is downloaded from Playwright's
CDN at install time; Orbit does not bundle it.

```bash
orbit browser-kit install    # build, upgrade or adopt the kit, then verify it
orbit browser-kit status     # version, browser revision, libraries, what workers get
orbit browser-kit verify     # rerun the loopback screenshot smoke test
orbit browser-kit remove --confirm
```

`orbit workspace init` offers the install when the repository has a web UI (a
`package.json` or an HTML file): it asks at a terminal, installs without
asking on `--install-browser-kit`, and otherwise only prints the command. It
never installs on its own.

## What workers receive

Once `verify` passes, Orbit adds three variables to every agent's
environment:

| Variable | Value |
| --- | --- |
| `PLAYWRIGHT_BROWSERS_PATH` | `~/.local/chromium-deps/browsers` |
| `PLAYWRIGHT_MODULE` | the pinned `playwright` package's `index.mjs` |
| `ORBIT_BROWSER_KIT_ENV` | `~/.local/chromium-deps/env.sh` |

A check sources `"$ORBIT_BROWSER_KIT_ENV"` in the shell that runs it, then
imports `chromium` from `"$PLAYWRIGHT_MODULE"`. Only that shell gets the
kit's unpacked libraries and fonts: Orbit never puts them on every worker's
`LD_LIBRARY_PATH`, where they could shadow what other tools load. Without a
verified kit (none installed, never verified, or changed since its last
pass), workers receive none of the three.

## Layout and pin

```text
~/.local/chromium-deps/
  env.sh          # what a check sources
  kit/            # npm project holding the pinned playwright package
  browsers/       # chromium_headless_shell-<rev>, ffmpeg-<rev>
  orbit-kit.json  # the last passing verify: Playwright version and revision
  verify/         # the smoke test's screenshot
  root/           # Linux without root only: unpacked libraries and fonts
  debs/           # Linux without root only: the downloaded packages
  fonts.conf      # Linux without root only, when the host has no fonts
  fontconfig-cache/
```

Each Orbit release pins one exact Playwright version, so evidence from every
host comes from the same browser build. `status` shows the pin beside the
installed version, and `install` moves the kit to it and deletes old browser
revisions. A kit built by hand at this layout is adopted: steps it already
satisfies are skipped, and an `env.sh` without Orbit's header line is kept
as it is.

Both executor sandboxes accept the kit without policy changes. `linux-bwrap`
and macOS `sandbox-exec` allow host reads, network (loopback included), and
writes to `/tmp` and the worktree's `.orbit/tmp/`. `linux-bwrap` gives each run
its own empty `/tmp`, so keep evidence in `.orbit/tmp/`. macOS denies reads of
browser profile directories under `~/Library/Application Support` (Chrome,
Chromium, Brave, Firefox), so the kit stays out of them.

## Linux libraries

macOS supplies Chromium's libraries and fonts. On Linux, `install` checks the
headless shell with `ldd` and closes any gap one of three ways:

- `--with-deps` runs `sudo playwright install-deps chromium` on the host. It
  asks for your password and installs the distribution packages system-wide,
  including some only headed Chromium needs.
- Without it, on Debian or Ubuntu, `install` downloads the packages Playwright
  names (`apt-get download`) and unpacks them into `root/` (`dpkg-deb -x`)
  without root. When `fc-list` finds no font it also unpacks fontconfig and
  DejaVu and writes `fonts.conf`; a server without fonts crashes Chromium in
  Skia/FontConfig. `env.sh` then exports `LD_LIBRARY_PATH` and
  `FONTCONFIG_FILE` ahead of the Playwright pair.
- Anywhere else it lists the libraries still unresolved and the package
  manager command that provides them, and does not verify until they resolve.

## Diagnose

`orbit doctor` reports the kit in three rows, each with its fix:

- `browser-kit`: missing (informational: many hosts never check UI),
  installed at another version than the pin, missing its browser revision or
  `env.sh`, or not verified.
- `browser-kit-libraries`: Linux libraries Chromium cannot resolve.
- `browser-kit-node`: whether `node` is on the login-shell PATH every agent's
  PATH ends with, and on the scheduler service's PATH. A `node` only in your
  interactive shell does not reach workers.

`verify` serves a page on loopback, screenshots it into `verify/smoke.png`
and records the pass in `orbit-kit.json`. Starting verification clears any
earlier pass: a timeout, failed spawn or failed smoke test leaves workers
without the kit until verification succeeds again. A failed smoke test
prints the likely cause:
a missing `lib*.so` or a Skia/FontConfig crash is a library or font gap, and
`Executable doesn't exist` means the module and browser revision are out of
step (`orbit browser-kit install` repairs both). When a worker's check fails
this way, the cause is the kit, not the code under test. Record it as host
evidence and fix the kit.

A host pass does not prove the worker path. The decisive check is a sandboxed
worker run (a disposable task, or the first UI task) that sources
`"$ORBIT_BROWSER_KIT_ENV"`, runs a browser check and attaches its screenshot.
`orbit run agent` (MCP `orbit_agent_invoke`, tool `orbit.agent.invoke`) runs on
the host outside Orbit's executor sandbox, so it is not that proof.

## Remove

`orbit browser-kit remove --confirm` deletes `~/.local/chromium-deps`.
Nothing else on the host refers to it, and workers stop receiving its
variables. Packages `--with-deps` installed system-wide stay installed.
