# Friction

Friction is a record of **something that made the work harder than it should
have been**. A confusing error, a missing flag, a build step that fails for
undocumented reasons, an API that behaves unlike its docs, a misleading prompt,
a dependency that breaks silently, a convention nobody wrote down.

The subject is not restricted to Orbit. Orbit's own tooling is the case the
default vocabulary is tuned for, because that is what Orbit seeds — but the
store constrains nothing about what a record is about, and the tag vocabulary is
yours to change.

**Friction is a report about the experience of doing the work. A task is the
work.** That is the line:

- "The test harness fails on a clean checkout with no useful error" → friction.
- "Fix the test harness so it works on a clean checkout" → task.

File the friction when you hit it. File a task when you're ready to fix it. Often
both, linked.

Not friction: ordinary user-requested work, a product bug you'd file anyway, or
a task whose content is merely vague — re-author that task instead.

Search the corpus first. The point of filing is a record someone finds *before*
re-diagnosing the same problem.

## Record shape

Records live in Orbit's store, keyed by `(workspace_id, friction_id)` — IDs are
workspace-local and monthly (`F<YYYY>-<MM>-<NNN>`), so the same ID in two
workspaces is two unrelated records. Reach them only through `orbit.friction.*`;
any `.orbit/frictions/` markdown tree is legacy evidence, and editing a file
there cannot change what a read returns. Creation is durable; `update` can replace the body and triage metadata. New records start `open` and may become `triaged` or `resolved`.

```bash
orbit tool run orbit.friction.add --input '{
  "title": "<the surface and the failure, max 120 chars>",
  "body": "<what happened, where, and why it caused friction>",
  "tags": ["<tag>"], "during_task": "<optional task id>",
  "model": "<agent-family>"
}'
```

`during_task` must name an existing task; an id that names none is refused as not
found instead of being recorded.

**Write the `title` yourself.** It is the record's handle everywhere the corpus
is scanned — `friction list`, the dashboard, and the search someone runs before
filing a duplicate. A handle that doesn't name its subject is invisible to that
search, which is how the same bug gets diagnosed twice. Name the surface and the
failure (`config loader rejects a value its own docs recommend`), not the shape
of your report.

Omitting `title` is allowed and derives one from the body: the opening line,
minus a leading section label, clamped to 120 characters. Derivation cannot
invent a subject the opening line doesn't state, so a body opening with a
section heading gets whatever that section's first sentence says.
`orbit friction update <ID> --title <title>` retitles an existing record
without replacing its body.

## Listing and JSON response contract

Over MCP or `orbit tool run`, list frictions with `orbit.search`, `kind:
"friction"`, and no `query`:

```bash
orbit tool run orbit.search --input '{"kind":"friction","status":"friction:open","model":"<agent-family>"}'
```

A listing covers every status unless a `friction:<open|triaged|resolved>`
status narrows it, can be filtered by `tag`, and is ordered by creation time
then ID. It returns up to 1000 hits by default (also the maximum `limit`); each
hit carries the full friction `record` (tags, model, `during_task`,
`rehome_to`), and a `notes` entry reports a listing cut off at `limit`.

The human `orbit friction list` command and the dashboard show search guidance
notes. `orbit friction list --json` remains a bare record array for
compatibility.

## Tags

```bash
orbit friction tags      # the vocabulary this workspace actually accepts
```

The seeded defaults:

| Tag | Use for |
| --- | --- |
| `automation` | Scheduler, auto-task, or delivery-automation friction |
| `build` | Build, format, and lint friction |
| `docs` | Stale or missing instruction and design docs |
| `history-diverged` | A rewritten branch history orphaned recorded automation state |
| `lifecycle` | Task lifecycle confusion or transition issues |
| `naming` | Naming drift or duplicated sources of truth |
| `policy` | Sandboxing and filesystem-profile surprises |
| `skill-guidance` | Misleading or incorrect agent instructions |
| `tooling` | Tool, CLI, or MCP failures |
| `other` | Fallback |

This list is seeded into the workspace's own tag file, not hard-coded. If your
workspace trips over things these don't describe — flaky infrastructure, data
quality, a specific subsystem — edit the vocabulary to match. A taxonomy that
sends everything to `other` is telling you it's the wrong taxonomy.

## Lifecycle

```bash
orbit tool run orbit.friction.update --input '{"id":"<ID>","status":"triaged","model":"<agent-family>"}'
orbit tool run orbit.friction.update --input '{"id":"<ID>","status":"resolved","model":"<agent-family>"}'
```

`update` also accepts `tags`, `body`, `title`, and `rehome_to`. Over MCP, use `orbit_friction_update`
with `status: resolved`; the separate CLI resolve operation is not an advertised
MCP tool. Include the selected `workspace` in every MCP example here.

When a task in the **same workspace** as the friction fixes the underlying
cause, give that task
`relations: [{"type":"resolves","target":"<friction-id>"}]`. Unqualified
friction IDs are workspace-local: auto-resolve looks up that ID only in the
task's workspace and records `resolved_by_task` when the task reaches `done`.
IDs are not global. Filing the task does not itself resolve anything — the
record stays open until the fix lands. Auto-resolve runs on the transition into
`done`, not on later writes to the done task: a friction reopened afterwards
stays open. A resolution that failed at that transition is recorded in the
task's history and retried by the task's next write.

A target that does not exist in this workspace and is not known to belong
elsewhere is dangling: audit-visible, and it does not block completion. A
target that exists only in another workspace on this host is **not**
dangling — completing the task is rejected with `friction_not_local`. Resolve
that friction from its owning workspace — `orbit friction resolve <id>` is the
operator CLI path; an agent instead runs `orbit tool run orbit.friction.update
--input '{"id":"<id>","status":"resolved"}'`, which stamps the same resolution
metadata — or land a covering task there. Do not count a foreign `resolves`
edge as coverage.

### Closing legacy records on a replica

A checkout re-registered as a replica keeps its host-local friction corpus.
Its friction reads still show those records, including reports authored before
the role change. They are separate from the owner's corpus: identical IDs on
the two hosts can name unrelated reports.

Close a local legacy report from that replica using the normal audited path:

```bash
orbit --workspace <replica-workspace> friction resolve <ID>
orbit tool run orbit.friction.update --input '{"workspace":"<replica-workspace>","id":"<ID>","status":"resolved","body":"<original report plus disposition evidence>","model":"<agent-family>"}'
```

The update must explicitly set `status: resolved`; it may include body, title,
tags, and a `rehome_to` disposition with `move: false`. Preserve the original
report when adding evidence, since `body` replaces it. Resolution keeps the
local ID, attribution, creation time, task reference, and first `resolved_at`
timestamp. It records the normal command audit and requires no connection to
the owner. Select the replica host and workspace explicitly over federated MCP.

This exception only closes existing records in the local workspace partition.
Additions, reopening, triage-only or metadata-only updates, and actual moves
remain refused on replicas, even with operator authority. Claimed workers
continue to use the owner route for friction writes.

## Re-homing a friction to its owning workspace

A friction is sometimes filed in the wrong workspace: a product workspace's
agent hits an Orbit or tooling defect, and the fix belongs to the workspace that
owns that code. A task filed in the reporting workspace would be wrong-lane, and
a `resolves` edge from the owning workspace cannot reach the record.

**Move it.** When the owning workspace is registered on this host, set
`rehome_to` on an update:

```bash
orbit tool run orbit.friction.update --input '{"id":"<ID>","rehome_to":"<name-or-ws-id>","model":"<agent-family>"}'
orbit friction rehome <ID> --to-workspace <name-or-ws-id>   # CLI spelling of the same move
```

Over MCP, use `orbit_friction_update` with the source `workspace`. Any other
edits in the same call apply first, then the move runs as one transaction:

- The owning workspace gets a copy under an ID it allocates, returned as
  `rehomed_as`. The copy keeps the title, reporter, creation time, task, triage
  status, and body, and adds a note naming the source record.
- The source is resolved. Its `rehome_to` names the owner, and a note in its
  body gives the new ID.

Tags the owning taxonomy does not define are dropped and listed in
`dropped_tags`. The move is refused when the record is already resolved, when
the same call also sets `status: resolved`, when the target is the current
workspace or is not registered, or when either checkout is a replica that
refuses coordination writes. A refused move writes nothing.

**Record the owner.** When you know which workspace owns the fix but it is not
registered here, add `"move": false`. That records only the `rehome_required`
disposition: the record stays open, and curation counts it as dispositioned,
not as uncovered. `"rehome_to": ""` clears a recorded disposition.

```bash
orbit tool run orbit.friction.update --input '{"id":"<ID>","rehome_to":"<owning-workspace>","move":false,"model":"<agent-family>"}'
```

## Reading the corpus

```bash
orbit friction list --status open
orbit friction show <ID>
orbit friction stats                          # friction rates over time
orbit search "<terms>" --kind friction        # lexical friction search
```

Frictions use lexical matching. Search by the words someone would actually have used.
→ [search.md](search.md)

Left alone, the corpus rots into duplicates. The seeded `friction-curation`
auto-task deduplicates it, verifies whether each survivor still reproduces, and
files fix tasks for the ones that do. → [auto-tasks.md](../../orbit-setup/references/auto-tasks.md)

## Rules

Never silently work around a problem worth recording. Never implement a large
design change inline — track it first. Name the concrete command, file, or
workflow that broke.
