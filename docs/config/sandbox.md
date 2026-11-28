# Sandbox configuration

## Sandbox write grants for the shared Cargo caches

Both OS sandboxes (macOS `sandbox-exec`, Linux Bubblewrap) let workers share one Cargo cache:

| Path | Inside a worker |
|---|---|
| `$CARGO_HOME/registry`, `$CARGO_HOME/git` | read-write |
| `$CARGO_HOME/.package-cache`, `.package-cache-mutate` | read-write (cargo's download locks, so concurrent workers stay serialized) |
| `$CARGO_HOME/bin` | read and execute, never write |
| `$CARGO_HOME/credentials.toml`, `$CARGO_HOME/credentials` | read-denied |
| `$CARGO_HOME`, `$CARGO_HOME/.global-cache` | read-only |

`$CARGO_HOME` is the child's variable if passed through `[execution.env].pass`, else `~/.cargo`. The grant applies only to profiles that already grant some write (`implementer`, `unrestricted`, `docs_writer`, …); read-only profiles such as `reviewer` and `pure_compute` get none. `reviewer` reads the workspace but writes nothing; `pure_compute` neither reads nor writes the workspace. On Linux, a cache path missing on the host is skipped, not created.

## Sandbox pseudo-tty allocation (macOS)

The macOS profile allows `pseudo-tty`, `/dev/ptmx` (read, write, ioctl), and read, write and ioctl on `/dev/ttys[0-9]+`, so `openpty`/`posix_openpt` work inside a worker (a test driving a CLI through a real terminal, say). Other PTYs remain subject to normal OS checks. Linux needs no equivalent rule.

---

## Host-health auto-task

`host-health` reads `orbit doctor --json` and warning/error service logs weekly
(on Mondays at 07:10 in the scheduler's local timezone). Like every other shipped
definition, it starts disabled in a new workspace; turn it on with
`orbit auto-task toggle host-health on`. In an existing workspace,
`orbit workspace sync` or `orbit workspace init` turns off a `host-health`
definition no operator has configured (its file still holds the bytes Orbit
wrote and `orbit auto-task toggle` never set its flag). `orbit init` leaves
existing definitions alone. Once an operator toggles it, or edits the file,
that choice decides. Only the workspace owner mints it. Turn it off or on,
and tune its schedule, with:

```sh
orbit auto-task toggle host-health off
orbit auto-task toggle host-health on
orbit auto-task update host-health --cron '10 7 * * 1'
# Or use an interval:
orbit auto-task update host-health --every-minutes 10080
```

Preparation pins the owner pass with `required_machine`. Collection never
repairs doctor checks, restarts services, edits config or reads credentials.
Doctor warnings/errors qualify on first sight; log signatures require three
distinct events across two completed collection windows on the same host within
fourteen days. Signatures remove volatile IDs, SHAs, home paths, ports and times.
An update or source-lag signature keeps its target version or commit.
The owner searches open tasks, tasks closed within 30 days and open frictions,
comments new counts on a covering task, and retains rejected mappings without
refiling them. It files at most five tasks per pass.

The same pass also records, still read-only, the installed Orbit version and
executable identity (path, SHA-256, and the source commit when a local-candidate
manifest matches), `orbit update --check`, how far a recorded source commit
trails the workspace integration branch, `orbit update --preflight`, and
`orbit migrate --dry-run` for each workspace on the host, plus doctor's
`retired-config-keys` findings. It does not install, apply migrations, adopt
generation authority, or remove retired keys. An available update, a lagging
source commit, a pending migration, and an authority gap those commands report
become human-crew chores. The report's `version` object and `migrations` array
hold the owner host's results. A follower on a different build is not one of
those chores: this pass cannot read that host.

**Service configuration.** There is no `host_health.units` config key. The
source of service names is the host's installed service-manager configuration:

| Host | Configuration read | Log inputs |
|---|---|---|
| Linux | Installed and loaded systemd **user** services: unit identity, fragment/drop-in paths, executable/subcommand, working directory and output destinations | `journalctl --user -u <discovered-unit> --since <window-start> --until <window-end> --output=json --no-pager`, selected configured log files |
| macOS | User LaunchAgents plists: `Label`, `Program`/`ProgramArguments`, `WorkingDirectory`, `StandardOutPath`, `StandardErrorPath` | Configured stdout/stderr files, or a bounded platform log query |

The pass discovers Orbit web, drain/run and clock/sweep launchers from those
properties, including renamed units and host-wide services. Configure or rename
services through the host's normal service manager; the next pass reads that
configuration. It also reads the global `~/.orbit/state/logs/orbit.jsonl` and rotated
archives, plus the existing workspace `.orbit/state/logs/` when present. It never dumps whole units/plists, environment properties or credential
files. Missing managers, inaccessible logs, rotation gaps and failed doctor
execution are reported as missing coverage. Other platforms need equivalent
configured service metadata and log access; unsupported collection is not
reported healthy.

**Follower collection.** Doctor is host-local; passing a remote workspace does
not run it on that host. Preparation accepts `required_machine` only for the
workspace owner. An entry that names a follower is refused, and writing that
machine ID into the instructions does not pin the task there. Automatically
pinned follower collectors are unavailable. Do not add a collector definition
that asks preparation to set `required_machine` to a follower, and do not enable
the owner aggregator on a replica. The shipped pass covers its owner only.
Every other host is missing coverage: the pass reports that gap and does not
treat it as healthy, as zero findings, or as version drift.

**Routes and resolution.** A verified Orbit code/default defect becomes an agent
bug with file/function evidence quoted at the inspected commit. Host, credential,
service-unit, OS and operator-only work goes to a configured [human crew](crews.md#human-crews).
If no human crew can be selected, the pass files a proposed setup chore tagged
`needs-operator` and `no-auto-approve`, requiring human assignment before any
promotion. Tags alone do not prevent dispatch; keep these fallback chores
proposed until an operator assigns a human crew. Every filing includes a
`## Resolve` section with exact host/shell-specific commands or an attached
`resolve.sh` (with `--dry-run` for a state-changing script), or concrete numbered
operator steps when no command exists, plus a verification command and expected
result. The collection pass never executes those repairs.

**Evidence and continuation.** Every completed pass attaches
`host-health-report.json` (signature groups, routes, dispositions, per-host
counts and coverage gaps) and `host-health-cursor.json` (source positions,
14-day counts, imported windows, rejected mappings and pending evidence).
Collection stops at 10,000 records or 8 MiB per host; a read or filing cap is a
successful partial pass with a fixed window and resume position. The next pass
drains pending groups before advancing, without recounting events. Excerpts are
limited to three lines/600 characters per signature and pass through Orbit's
text redaction before entering an artifact; if that route is unavailable,
excerpts are omitted. Never print `~/.zprofile`, `~/.orbit/clock.env` or credential
file contents. Raw logs are never attached. Empty passes still store both
artifacts.
