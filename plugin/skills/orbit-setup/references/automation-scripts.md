# Automation: workspace scripts

## Contents

- [Workspace scripts](#workspace-scripts)
  - [Grants](#grants)
  - [Grant states](#grant-states)
  - [Grant requests](#grant-requests)
  - [Verify a grant](#verify-a-grant)

## Workspace scripts

A workspace script is a program that a job step runs as `target: script:<name>`.
Its manifest lives at `.orbit/resources/scripts/<name>.yaml`, and the manifest's
`entry` names the program file in the workspace. A script step runs only while
the script holds an operator grant on the host that runs it. Granting runs
nothing; the step runs when a job reaches it.

A manifest whose entry is missing or invalid is unavailable; jobs that name
it refuse with an error naming the manifest. Other jobs can still run. Repair it, or delete a retired
script's manifest and run `orbit script revoke <name>`. The retired ux-rules
routine left one: if the workspace set it up, delete
`.orbit/resources/scripts/ux_rules_runner.yaml` and run
`orbit script revoke ux_rules_runner`.

`orbit doctor` reports it under
`retired-scripts`, and a manifest that fails to load under `script-grants`.

### Grants

A grant is the operator's host-local consent to run one script at one set of
content. It binds to the digests of both the manifest and the entry file.
Editing either file makes the grant `stale`: the script does not run until an
operator grants it again at the new digests.

Only operators grant and revoke. An agent session can list, show, and read
history, but must not grant, even when it can edit the manifest or the program.

```bash
orbit script grant <name>                          # grant at the current digests
orbit script grant <name> --read-root /srv/reports --write-root /srv/out
orbit script grant <name> --follower               # grant on a pull follower, not the owner
orbit script revoke <name>                         # revoke this host's grant
```

| Grant flag | Meaning |
|---|---|
| `--network` | Network the grant allows: `none`, `https` or `any`. Default: what the manifest requests. |
| `--read-root <path>` | Extra absolute read root to grant. Repeatable. |
| `--write-root <path>` | Extra absolute write root to grant. Repeatable. |
| `--env-pass <NAME>` | Extra environment variable name to pass to the script. Repeatable. |
| `--follower` | Grant for a pull follower host instead of the workspace owner. |
| `--output-max-files <n>` | Most declared output files one step may keep, 1 to 64. Default 8. |
| `--output-max-file-bytes <b>` | Largest output file one step may keep, up to 64 MiB. Default 1 MiB. |
| `--memory-max <size>` | Memory ceiling of the script's own scope (`MemoryMax=`): a byte count with an optional `K`, `M`, `G` or `T` suffix (powers of 1024), at least 1 byte. Percentages and `infinity` are refused. Default: `machine.worker_memory_max` (`50%` of RAM). |
| `--tasks-max <n>` | Process and thread ceiling of the script's own scope (`TasksMax=`), at least 1. Default: `machine.worker_tasks_max` (`4096`). |
| `--cpu-quota <percent>` | CPU ceiling of the script's own scope as a percentage of one core (`200` is two cores), at least 1. Default: `machine.worker_cpu_quota` (unset: no CPU limit). |

The three scope flags set the script's own Linux systemd scope
(`orbit-script-*`). They apply only to Linux script steps; a macOS script step
has no memory or task limits. Each flag is capped at its host setting, so a grant
can narrow a script's scope but never widen it past the host. `--memory-max`
narrows `MemoryMax=` only when it is below `machine.worker_memory_max`, and also
narrows `MemoryHigh=` when it is below `machine.worker_memory_high`.

`--tasks-max`
is capped at `machine.worker_tasks_max`. `--cpu-quota` is capped at
`machine.worker_cpu_quota` when that is set; when it is unset, the grant's quota
still applies, so a grant can add a CPU limit the host does not set. systemd
enforces `--cpu-quota` only where it delegates the cpu controller. A value of 0
is refused. Each grant states its scope ceilings again, so a re-grant without
the flags returns each one to its host default.

Every read and write root, from the flags or the manifest's `requests`, must
already exist, sit outside the credential paths (`~/.ssh`, `~/.config/gh`, the
keychains, …), and pass a probe by the granting process: it lists the directory
(or opens a file root), and for a write root creates and removes a file. A root
that fails refuses the grant, names the path, and nothing is granted.

Before
every run, the process that runs the job repeats the probe; a root that is
missing or denied then refuses the step with `script_root_unavailable`, naming
the path, and nothing runs. Orbit never creates a granted root.

A script that declares `files.outputs` in its manifest keeps the regular files it
writes at the top level of `out/`, and later steps read them as
`steps.<id>.files` (`name`, `path`, `bytes`, `media_type`, `sha256`). The
manifest's caps must fit the grant: asking for more than the grant allows
refuses the step with `grant_insufficient` and names the flag to grant again
with. Each grant states its output limits again, so granting without the flags
returns to the defaults.

A step's whole set never exceeds 256 MiB. Output over
the caps, a symlink, a subdirectory or an unsafe file name fails the step and
exposes none of its files.

To file a task that carries those files, a later step of the same job runs the
shipped `attach_script_files` activity with `task_id` and `step` (the script
step's id), and optionally `files` (names to attach; all by default) and
`path_prefix` (prepended to each artifact path). It attaches each file as a
task artifact from bytes that still match the recorded SHA-256.

A changed file,
a name outside the set, a file over the 1 MiB task-artifact limit or a script
step that is not a direct, earlier step of the job refuses the step and
attaches nothing.

A manifest's `files.inputs` lists `{from, as}` entries. `from` may read the
step input (`{{ input.<field> }}`) and resolves, symlinks followed, to a file
or directory inside the step's cwd tree or a granted read root (`--read-root`).
Each one appears read-only at `in/<as>` (`$ORBIT_SCRIPT_IN/<as>`). An input
that is missing, resolves anywhere else, or overlaps the workspace's `.orbit/`
or a credential path refuses the step before it runs with
`script_file_input_unavailable`, naming the input.

Each attempt records every
input's name, source path and SHA-256 (for a directory, a digest over its whole
tree) as `input_files` in the `--json` output of `orbit run show` and
`orbit script history`.

A granted script step runs on Linux under `linux-bwrap-script-v1` and on macOS
under `macos-sandbox-script-v1`; other platforms refuse the step with
`script_sandbox_unavailable`, and so does a step started inside another sandbox
(a worker's own Seatbelt or Bubblewrap sandbox), which never runs nested.

The
`RLIMIT_CPU` soft limit is the timeout rounded up to whole CPU seconds per
process, with a minimum of 1 CPU second, and its hard limit is 10 CPU seconds
later, capped at the hard limit inherited by the wrapper. The script can raise
its CPU soft limit up to that hard limit. `RLIMIT_NOFILE` is 1024 for both its
soft and hard limits, capped at the wrapper's inherited hard limit, so the
script cannot raise it. On Linux, network access is off unless allowed by the
grant.

`--network https` shares the host network but admits outbound TCP to
port 443 only (it needs Landlock ABI 4, so an older kernel refuses the step),
and `--network any` shares it with no rule.
Only the variables named with `--env-pass` (or in the manifest's
`requests.env_pass`) reach the script, copied from the environment of the
process that runs the job; `ORBIT_*` names and the profile's own `PATH`,
`HOME`, `TMPDIR`, `LANG` and `TZ` are never passed.

A passed value is redacted
from the step's log, its output and `orbit run show` / `orbit script history`.
A manifest asking for more network or another variable than its grant refuses
with `grant_insufficient`. If the sandbox itself fails, see
[Linux sandbox](linux-sandbox.md).

On macOS a script step never has network: a grant with `--network https` or
`any` refuses the step with `script_sandbox_unavailable`. The macOS profile
allows the same paths as the Linux one, but the script sees its scratch
directory at its host path (use `$ORBIT_SCRIPT_IN`, `$ORBIT_SCRIPT_OUT`,
`$HOME` and `$TMPDIR`, never `/orbit-script`), and declared inputs are copied
into `in/`, so they count against the scratch size limit.

It has no memory or
task limits, a timed-out descendant that calls `setsid` can outlive the step
(still confined), and file metadata outside the grant stays visible; `orbit
doctor` lists these gaps in its `script-sandbox` row.

With
`machine.worker_containment = "strict"`, every script step on macOS is refused.
macOS privacy permissions (TCC) apply on top of the profile and belong to the
process that runs the job (the Orbit clock's launch agent, or the `orbit`
binary it runs), not to the terminal that granted. A root they deny refuses the
step with `script_root_unavailable`, naming the path and the System Settings >
Privacy & Security pane to allow it in.

### Grant states

`orbit script list` and `orbit script show` report one of these states:

| State | Meaning |
|---|---|
| `granted` | The grant's digests match the current manifest and entry file. |
| `stale` | A verified grant whose digests no longer match: one of the files changed after granting. |
| `ungranted` | A manifest with no grant. |
| `orphaned` | A grant whose manifest no longer exists. |
| `unavailable` | The manifest could not be loaded; its diagnostic names the error and remedy. |
| `witness_refused` | The stored grant is not trusted because its witness is missing or invalid, its row does not match the witness, or its timeout ceiling is outside `1..=4294967295` seconds. Review it and run `orbit script grant <name>` to re-authorize it. |

### Grant requests

When a script step is refused with `grant_missing`, `grant_stale` or
`grant_insufficient`, Orbit also raises a `script_grant` decision request, and the
refusal message names it. The request carries the script, the digest it would
grant, the refused grant's digest, the diff against the last granted bytes, and
the requested against granted capabilities. Another refusal at the same digest and
capability set reuses the open request.

After an edit, the next refusal raises a new
request and supersedes the old one. Requests live on a `someday` task per script,
titled ``Script grant requests: `<name>` ``, which agents never pick up. A witness
failure, a missing sandbox or a changed interpreter raises no request, because a
grant would not clear it.

```bash
orbit decision list --gate script_grant     # open grant requests
orbit decision show <id>                     # script, digests, capabilities and the diff to review
orbit decision answer <id> grant             # write the grant at that digest (operator only)
orbit decision answer <id> reject            # leave the script ungranted
```

A `grant` answer writes the grant exactly as requested, with the answering human
as grantor. It is refused, and writes nothing, when the script's bytes or requested
capabilities have changed since the request was raised. The same answer also comes
from a registered [decision channel](plugins-decision-channels.md#decision-channels) (provenance
`decision-channel <name>`) or from the dashboard. An agent session or plugin
callback can neither answer the request nor write a grant.

The dashboard's **Automation › Scripts** view lists every script with its state
(adding `revoked` for a script whose grant was withdrawn after it ran), digests,
the commit that made it stale, requested against granted capabilities, its open
request and the diff. An operator session can grant from there; the action is
bound to the digest shown, so bytes that change after the page loaded are refused.
A grant answer binds the catalog bytes in the workspace's primary checkout.

A step
that ran from a worktree with different bytes is refused as superseded until those
bytes land.

### Verify a grant

```bash
orbit script list                  # NAME, ENTRY, GRANT, DIGEST for every script
orbit script show <name>           # manifest and entry paths, current and granted digests, network, grantor, detail
orbit script history <name>        # recent attempts, newest first, refusals included (--limit, default 20)
```

1. `orbit script list` should show `granted` in the GRANT column for each script
   you expect to run. `stale`, `ungranted` or `orphaned` means the step will
   not run.
2. `orbit script show <name>` should report `grant state: granted`, with the
   current manifest and entry digests equal to the granted ones. A difference
   after an edit is the staleness above; grant again to accept the edited files.
3. After a run, `orbit script history <name>` lists each attempt, including the
   refused ones.
