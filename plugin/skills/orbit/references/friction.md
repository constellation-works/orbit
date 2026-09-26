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

`orbit.friction.list` returns a JSON array of friction records by default,
including when any combination of filters produces no records. This is the
legacy contract and is stable for array consumers.

Callers that want search guidance can explicitly send
`"response_mode": "with_notes"`. That mode always returns an object with the
same two fields: `records` is the record array and `notes` is a string array.
An empty multi-word substring search may include guidance in `notes`; matches,
one-word misses, and other empty filtered results return an empty `notes`
array. Other `response_mode` values are rejected.

The human `orbit friction list` command and the dashboard opt into notes so
they can show that guidance. `orbit friction list --json` remains a bare record
array for compatibility; the notes envelope is available through the tool/MCP
input above.

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

`update` also accepts `tags`, `body`, and `rehome_to`. Over MCP, use `orbit_friction_update`
with `status: resolved`; the separate CLI resolve operation is not an advertised
MCP tool. Include the selected `workspace` in every MCP example here.

When a task in the **same workspace** as the friction fixes the underlying
cause, give that task
`relations: [{"type":"resolves","target":"<friction-id>"}]`. Unqualified
friction IDs are workspace-local: auto-resolve looks up that ID only in the
task's workspace and records `resolved_by_task` when the task reaches `done`.
IDs are not global. Filing the task does not itself resolve anything — the
record stays open until the fix lands.

A target that does not exist in this workspace and is not known to belong
elsewhere is dangling: audit-visible, and it does not block completion. A
target that exists only in another workspace on this host is **not**
dangling — completing the task is rejected with `friction_not_local`. Resolve
that friction from its owning workspace — `orbit friction resolve <id>` is the
operator CLI path; an agent instead runs `orbit tool run orbit.friction.update
--input '{"id":"<id>","status":"resolved"}'`, which stamps the same resolution
metadata — or land a covering task there. Do not count a foreign `resolves`
edge as coverage.

## Re-homing a friction to its owning workspace

A friction is sometimes filed in the wrong workspace: a product workspace's
agent hits an Orbit or tooling defect, and the fix belongs to the workspace that
owns that code. A task filed in the reporting workspace would be wrong-lane, and
a `resolves` edge from the owning workspace cannot reach the record.

**Record the owner.** When you know which workspace owns the fix but cannot
move the record from here, set `rehome_to`. This is the `rehome_required`
disposition. The record stays open, and curation counts it as dispositioned,
not as uncovered. An empty string clears it.

```bash
orbit tool run orbit.friction.update --input '{"id":"<ID>","rehome_to":"<owning-workspace>","model":"<agent-family>"}'
```

**Move it.** When the owning workspace is registered on this host, move the
record:

```bash
orbit friction rehome <ID> --to-workspace <name-or-ws-id>
orbit tool run orbit.friction.rehome --input '{"id":"<ID>","to_workspace":"<name-or-ws-id>"}'
```

Over MCP, use `orbit_friction_rehome` with the source `workspace`. The move is
one transaction:

- The owning workspace gets a copy under an ID it allocates. The copy keeps the
  title, reporter, creation time, task, triage status, and body, and adds a note
  naming the source record.
- The source is resolved. Its `rehome_to` names the owner, and a note in its
  body gives the new ID.

Tags the owning taxonomy does not define are dropped and listed in
`dropped_tags`. The move is refused when the record is already resolved, when
the target is the current workspace or is not registered, or when either
checkout is a replica that refuses coordination writes. A refused move writes
nothing.

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
