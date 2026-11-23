# Configuration: fleet notices and host capability labels

## Contents

- [Fleet notices, attention, evolution ledger and failing jobs](#fleet-notices-attention-evolution-ledger-and-failing-jobs)
  - [Failing-job warning](#failing-job-warning)
  - [Resident evolution ledger](#resident-evolution-ledger)
  - [Fleet notices](#fleet-notices)
  - [Attention](#attention)
- [Host capability labels](#host-capability-labels)

## Fleet notices, attention, evolution ledger and failing jobs

Fleet notice limits are fixed (below). The other settings are ordinary layered settings: a workspace value overrides
the global value, and the built-in default fills the rest. Set one for a
workspace with `orbit config set <key> <value>`, or for every workspace on
the host with `orbit config set --global <key> <value>`. Verify any one
with `orbit config get <key>`, and inspect the effective values with their
sources with `orbit config show` or `orbit config show --all`.

The types,
defaults and ranges below match `orbit config keys --json` for the
installed version; check that command before applying version-dependent
values.

### Failing-job warning

The dashboard Jobs catalogue shows a failing badge on a job after 5
consecutive failed runs. The badge is display only: it never pauses, disables
or changes a job. `jobs.failure_streak_warn` loads with a deprecation warning,
is ignored, and stops being recognised after 2026-12-08. Delete it.

### Resident evolution ledger

Resident checkpoints record capability gaps in the evolution ledger, which
`orbit evolution` and `orbit resident digest` read. Nothing files evolution
proposal tasks automatically: the weekly evolution pass was removed.
A declined record reopens early only when its cost inside the window doubles.
The resident digest reads 14 days of ledger cost by default, a decline
measures the same 14 days, and a record stays declined for 30 days after its
proposal task is rejected.

`resident.evolution.window_days` and
`decline_suppress_days` load with a deprecation warning, are ignored, and stop
being recognised after 2026-12-08.
`min_occurrences`, `min_distinct_cycles`, `min_distinct_mandates`,
`min_cost_minutes` and `max_proposals` belonged to the removed pass: they load
with a deprecation warning, are ignored, and stop being recognised after
2026-12-07. Delete them.

### Fleet notices

Fleet notices are short, expiring, attributed announcements. Their limits
are fixed; no key changes them:

- Default expiry: 72 hours for a warn or incident, 12 for maintenance, 168
  for info. A managed agent's notice defaults to, and is capped at, 24 hours.
  A resident mandate's warn defaults to 72 hours (at most 168 hours), and its
  status line expires at twice the mandate's heartbeat (or 168 hours for a
  control line left when paused, taken over, or stopped).
- Longest expiry: 720 hours for an operator; 168 (warn, incident, info) and 72
  (maintenance) for an orchestrator; 168 for a resident mandate's warn.
- Size: titles up to 120 characters, bodies up to 4096 UTF-8 bytes, at most
  8 evidence references.
- Open notices: 20 per workspace, 10 global per owner host, 2 per managed
  task, 10 per resident mandate.
- Post rates: 5 per managed task in 24 hours (corroborations included), 30
  per orchestrator crew per hour, 30 per resident mandate per hour.
- Retention: closed and expired notices stay listable for 30 days.
- Agents: managed agents always may post and close their own notices
  through their bound task.
- Brief: opted-in managed activities always receive up to 5 open notices
  (warn notices and residents' status lines are excluded), each body cut to
  600 characters; a read that takes over 2000 ms gives an unavailable brief.

The former `notice.*` keys load with a deprecation warning, are ignored, and
stop being recognised after 2026-12-08. Delete the `[notice]` table.

### Attention

The attention feed that reports work needing a human or operator has fixed
bounds: a delta-polling cursor stays valid for 24 hours, failed runs are read
over the last 48 hours, a run counts as stuck after 120 minutes of wall clock,
the base-branch CI reading is cached for 60 seconds, each source read has 10
seconds, a cross-host call has 20 seconds, each workspace contributes at most
200 candidates, and an item's severity steps up at 24 and 72 hours.

The
`attention.*` keys load with a deprecation warning, are ignored, and stop being
recognised after 2026-12-08. Delete them.

## Host capability labels

`machine.labels` declares the capabilities available on a host. Set it in the
global configuration file under `[machine]`:

```toml
[machine]
labels = ["gpu", "cuda-12"]
```

Each host also carries its operating system as an implicit label: `linux`,
`macos` or `windows`. You do not need to add the OS label to `machine.labels`.
Orbit does not detect capabilities; it uses the labels you configure and the
host's OS.

Run `orbit doctor` to verify the host's effective labels in the `host-labels`
row. The row reports `machine.labels` together with the implicit OS label.

A task's `requires_labels` lists every label a host must carry to start it. A
host must match **all** of those labels. If no live host carries them, task
show (`Placement:`), task list and `orbit run readiness` report a placement
wait such as:

> waits for a host labelled `gpu` (requires_labels): no registered serving host carries them

The task stays in the backlog while it waits.

Map tags to required capabilities in the owning workspace's configuration:

```toml
[workflow.tag_requires_labels]
ui = ["visual-fixture"]
```

Every matching tag adds its labels to the task's explicit `requires_labels`;
all are required. Tags match after trimming and lowercasing, and the labels
use the same normalization as `machine.labels`. The default map is empty.
Workspace entries replace global entries with the same key spelling; `ui = []` disables an
inherited mapping. Invalid labels, blank tags and duplicate normalized tag
keys are refused at load.

Derived labels apply to existing tasks at their
next admission and are not stored on the task. Clearing `requires_labels`
does not remove a tag's requirements. Local admission, placement, crew draws,
pull admission and census affinity use this union. The owner supplies pull
policy, so a follower cannot weaken it with its own map. The `ui` browser-kit
check remains separate and mandatory.

Before declaring `visual-fixture` in a host's `machine.labels`, verify the
workspace's fixture backend inside that host's managed worker runtime, using
a disposable home and workspace. A browser-kit verification or host-shell run
alone does not prove the backend is available to workers. Hosts whose worker
runtime denies it must omit the label. Labels describe capabilities, not OSes;
if no serving host carries a required label, the task waits.
