# Configuration: execution timeouts, environment, filesystem profiles and executors

## Contents

- [Activity wall-clock timeouts](#activity-wall-clock-timeouts)
- [Environment passthrough](#environment-passthrough)
- [Policies and filesystem profiles](#policies-and-filesystem-profiles)
- [Executor definitions](#executor-definitions)
  - [`host_os`: which hosts may run this executor](#host_os-which-hosts-may-run-this-executor)
  - [`auth_probe`: re-admitting a pull-drain provider after auth failures](#auth_probe-re-admitting-a-pull-drain-provider-after-auth-failures)
  - [`inspection_args`: tools for pinned-source inspection runs](#inspection_args-tools-for-pinned-source-inspection-runs)
  - [Shared Cargo-cache sandbox grants](#shared-cargo-cache-sandbox-grants)

## Activity wall-clock timeouts

Set CLI agent activity budgets by exact activity name in the workspace's
`.orbit/config.toml` (or globally for a host-wide policy):

```toml
[workflow.activity_timeouts]
agent_implement = 28800 # 8 hours; shipped default is 21600 seconds (6 hours)
```

`workflow.activity_timeouts` is a table of activity names to integer seconds,
from `1` through `43200` (12 hours), inclusive. Zero, negative, non-integer and
larger values fail configuration loading; the error names the setting. Names
must be non-empty and contain no whitespace. Match the activity name, such as
`agent_implement`, rather than a pipeline step ID or provider name. Custom CLI
agent activities use the same table.

Unmapped activities retain their declared
timeout, including the runner fallback when the declaration is zero. An empty
table overrides nothing. A workspace table replaces the global table, so copy
any global entries you want to retain.

The override is resolved once per activity step. Implementation continuations
and response corrections consume the remaining resolved budget rather than
starting a fresh full budget. An admitted review keeps its `review.minutes`
bound ahead of activity overrides. Restart long-running Orbit processes after an
edit so they load the new policy.

The shipped task gate allows 13 hours for child delivery and reserves its
footprint for the same interval: the maximum 12-hour implementation override
plus a bounded 1-hour delivery/review tail. The auto pipeline allows 15 hours
for the gate (1-hour admission, 13-hour delivery and 1-hour tail). Child waits
and reservation TTLs admit up to 86400 seconds (24 hours). Recovery and review
activities keep their separate declared budgets; they do not enclose
implementation.

Custom jobs and explicit shorter wait inputs can still cancel
a child earlier; keep enclosing waits and reservation TTLs consistent with the
activity budgets you choose.

Inspect or replace the effective table with installed commands:

```bash
orbit config get workflow.activity_timeouts
orbit config set workflow.activity_timeouts '{ agent_implement = 28800 }'
orbit config show --all
```

## Environment passthrough

Agent subprocesses always start from a **cleared** environment plus the
allowlist — the Orbit process's environment is never inherited, and that is not
configurable. The seeded baseline covers `HOME`, `PATH`, `TMPDIR`, `USER`, and
the provider home directories. Credentials are opt-in: add `GITHUB_TOKEN` or a
provider API key explicitly if agents need them.

The shipped default includes none of `CLAUDE_CODE_OAUTH_TOKEN`,
`ANTHROPIC_API_KEY` or `CODEX_API_KEY`. Choose shared `pass` or a global
per-crew `env` mapping for the intended authentication route; see
[Provider credentials](providers.md#provider-credentials) for variable names,
Codex API-key examples, unset-name warnings and service verification.

```toml
[execution.env]
pass = ["HOME", "PATH", "TMPDIR", "USER", "GITHUB_TOKEN"]
```

## Policies and filesystem profiles

A policy defines the filesystem profiles activities run under.

```bash
orbit doctor fs-access <profile> <path>    # dry-run a workspace-relative path against the profile rules
```

Shipped profiles: `reviewer` (reads the workspace, writes nothing),
`pure_compute` (neither reads nor writes the workspace, so pick it only for an
activity that needs no workspace files), `docs_writer` (scoped writes),
`implementer` (workspace writes), `unrestricted`. A read-only activity that
opens workspace files should use `reviewer`; `pure_compute` denies those reads too.

The trap worth knowing: an activity that omits `fsProfile` falls back to
the `unrestricted` profile. A read-only activity must declare its profile
explicitly — silence is not a safe default here.

The operating-system boundary matters: macOS uses `sandbox-exec`; Linux uses
Bubblewrap to enforce allowed writes while leaving host reads and network
available. Other platforms have no backend: dispatch refuses a shipped agent
executor there unless its `spec.sandbox` is `off`, and Windows runs Orbit
inside WSL2. A read-only profile is not a claim of network isolation or
private host reads.
See [first-run-machine.md](first-run-machine.md) for the Linux prerequisite.

## Executor definitions

Four executor settings decide whether a crew can run on a host and how it
recovers. They live in the installed executor definition (YAML, beside
`sandbox` and `model_flag`), not in `config.toml`, so `orbit config` never
shows them. Edit the installed definition to change them: reseeding preserves
an operator-set value and only fills in fields the install omits, and custom
executors outside the shipped set are never rewritten.

### `host_os`: which hosts may run this executor

A non-empty subset of `linux`, `macos` and `windows`. An empty list or any
other name is refused when the definition loads. Omit `host_os`, or set it
null, and the executor runs on every host; no shipped executor pins it.

```yaml
spec:
  host_os:
    - macos
```

A task's effective OS requirement is its `os:` tags intersected with its
assigned crew's executor `host_os`. An empty intersection is refused when the
task is created or updated, and an explicit crew this host cannot run stays
assigned and waits instead of being redrawn onto another crew.

Verify with `orbit doctor providers`: an executor `host_os` excludes this
host reports `unsupported on this OS`, even when its binary is absent
(`not installed` is only for an executor this host may run whose command is
missing), and the full `orbit doctor` agrees. Waiting tasks name the mismatch
(`host_os_mismatch`); filter candidacy with
`orbit task list --runs-on linux|macos|windows`.

### `auth_probe`: re-admitting a pull-drain provider after auth failures

Only a pull drain's auth-excluded providers run the probe: never at drain
start, for healthy crews, after the window closes, or for capacity or refusal
exclusions. The first attempt is due ten minutes after the failed leaf, then
backs off (twenty, then thirty minutes, capped). A pass re-admits that
provider account's crews within the window and never clears any other
exclusion; a crew on its own account is excluded, probed and re-admitted
alone.

Without a probe the exclusion lasts the whole window: re-login and
start a new drain. Only the shipped definition for one provider declares one.

Opt in by adding the declaration to the installed definition and starting a
drain. The probe reuses the executor's resolved command, replaces the activity
argv with `args`, and requires exit zero plus a marker line on stdout;
`timeout_seconds` is 1–120. Verify with `orbit doctor`
(`provider-auth:<provider>`, or `provider-auth:<provider>/<crew>` for a crew
account) and `orbit run show <drain>`: both name the account, exclusion time,
re-login hint and next probe time.

Doctor reads the record and calls no
provider.

### `inspection_args`: tools for pinned-source inspection runs

The arguments appended to the executor's argv for an inspection run (task
pilot, escalation triage), so the CLI's own file-read
and shell tools work unattended. An empty list means the static `args`
already enable those tools; omitting the field means no known inspection
surface. Inspection draws skip crews without it, and an explicit crew that
names one fails before launch with `InspectionToolsUnavailable`. Read-only is
enforced by the `reviewer` filesystem profile, not by these arguments.

Two shipped definitions declare it (an empty list for one, feature flags for
the other). Declare the arguments on the installed definition once the CLI's
native tools are known to work non-interactively under the `reviewer`
profile; an installed omission takes the shipped default of the same name,
while a set value wins.

### Shared Cargo-cache sandbox grants

Both OS sandboxes let workers share one Cargo cache: the registry, git
checkouts and download locks under `$CARGO_HOME` are read-write, `bin` is
read and execute only, credentials are read-denied, and the cache root itself
is read-only. `$CARGO_HOME` is the child's variable when
`[execution.env].pass` names it, else `~/.cargo`.

There is no executor field
for this: the grant applies only to profiles that already grant some write
(`implementer`, `unrestricted`, `docs_writer`, …) and never to read-only
profiles: `reviewer` reads the workspace but writes nothing, and
`pure_compute` neither reads nor writes the workspace, so neither gets the
grant. On Linux a cache path missing on the host is skipped, not created.

To use a non-default cache location, pass it through
`[execution.env].pass`; to change what a worker may write, change the
activity's filesystem profile, not the executor. Dry-run a path with
`orbit doctor fs-access <profile> <path>`.
