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

## Tags

```bash
orbit friction tags      # the vocabulary this workspace actually accepts
```

The seeded defaults:

| Tag | Use for |
| --- | --- |
| `build` | Build, format, and lint friction |
| `docs` | Stale or missing instruction and design docs |
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

`update` also accepts `tags` and `body`. Over MCP, use `orbit_friction_update`
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
that friction from its owning workspace (`orbit.friction.resolve`, or a
covering task there). Do not count a foreign `resolves` edge as coverage.

## Reading the corpus

```bash
orbit friction list --status open
orbit friction show <ID>
orbit friction stats                          # friction rates over time
orbit search "<terms>" --kind friction        # lexical; frictions are never embedded
```

Frictions are searchable but not vectorized, so they stay lexical even under
`--hybrid`. Search by the words someone would actually have used.
→ [search.md](search.md)

Left alone, the corpus rots into duplicates. The seeded `friction-curation`
auto-task deduplicates it, verifies whether each survivor still reproduces, and
files fix tasks for the ones that do. → [auto-tasks.md](setup/auto-tasks.md)

## Rules

Never silently work around a problem worth recording. Never implement a large
design change inline — track it first. Name the concrete command, file, or
workflow that broke.
