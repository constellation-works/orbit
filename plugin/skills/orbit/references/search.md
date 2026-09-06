# Search

`orbit search` finds project context by topic, literal phrase, or related task
ID across three corpora: tasks, docs, and frictions. It is corpus retrieval, not
structural traversal — for callers, refs, implementors, or symbol selectors,
read files with the provider-native file-read tool or use `rg`.

The query surface is `orbit.search` (MCP `orbit_search({...})`, CLI
`orbit tool run orbit.search --input '{...}'`). Include `model` for provenance.
The *lifecycle* surface — `orbit semantic install|uninstall|stats|index` —
manages the embedding companion and is not a way to query anything.

```bash
orbit search "slow inference after model swap" --limit 5          # lexical, all corpora
orbit search "scheduler" --tag perf --kind all                    # --tag is AND when repeated
orbit search path src/lib.rs --kind all                           # applicability lookup
orbit search "agent loop deadlock" --hybrid --kind task --limit 5 # lexical + cosine
orbit search similar "<task-id>" --limit 5                        # MCP: {"semantic":"<task-id>"}
```

**Applicability (`search path`) filters tasks only.** A task matches when one of
its `context_files` selectors overlaps the query path — `file:`, `dir:`, and
`symbol:<file>#<name>:<kind>` all collapse to a plain path first, and
containment is bidirectional, so querying a parent directory matches every
selector beneath it. Docs are content-indexed and never match by path; frictions
carry no path scope at all. Both are absent from `search path` results
regardless of `--kind`.

**`--status` takes `kind:value` tokens** (`--status task:open,doc:active`). Bare
tokens are rejected because statuses collide across corpora.

**Index coverage:** lexical covers all three corpora. Vector search covers task
fields and docs once `orbit semantic index --kind <kind>` has run; frictions are
never embedded, so they stay lexical even under `--hybrid`. Missing vectors under
`--hybrid` fall back to lexical with a note rather than failing — and if the
companion isn't installed at all, fall back to lexical and continue. Never run
`orbit semantic install` without operator consent.

## Two different dedupe checks

They are not interchangeable, and using the wrong one silently finds nothing:

- **Before creating a task** — the task doesn't exist and has no vectors, so
  query the *text*: `--hybrid --kind task` on the title and description.
- **After loading a task** — the task exists, so query by identity:
  `search similar "<task-id>"`, or `{"semantic": "<task-id>"}` over MCP. This is
  the pickup-time check that surfaces prior decisions the author never linked.

There is no `orbit.search.similar` tool. `similar` is a subcommand of the single
`orbit.search` surface, and it requires task vectors to exist.

**Where else it earns its keep:** "where did we decide X" as `--kind all`. For
exact identifiers, paths, and error strings, plain lexical is cheaper and more
predictable than semantic.

**Stop rule:** if one well-formed query returns useful hits, stop and inspect.
Don't chain rewrites chasing a higher score.

Results carry `mode`, `kind`, `notes`, and `results[]` with some of
`id`/`path`/`title`/`summary`/`status`/`best_field`/`snippet`/`score`/`matched_by`.
Scores are relative ordering, not confidence — read the snippet and matched field
before judging relevance.

## Corpora

**Tasks** — the full lifecycle record, embedded when indexed.

**Docs** — reviewed Markdown under configured `[docs].roots` (default `docs/`):
designs, patterns, domain notes, glossaries, runbooks. Orbit walks the roots on
demand and indexes anything with valid frontmatter. Authoring a doc, registering
a root, or migrating legacy files: [docs-corpus.md](docs-corpus.md).

**Frictions** — records of what made the work harder. Lexical only.
→ [friction.md](friction.md)

**Historical decision documents**, when present under docs roots, are ordinary
docs, not a separate corpus or an authority source. Evaluate retrieved claims
against current requirements and evidence. The retired `--kind adr` is rejected.

## Scope and hidden history

Ordinary searches hide closed task/friction history. `all: true` includes
normally hidden statuses for the selected kind; explicit `status` tokens override
that default. Use a bounded all-status pass before concluding a repair was never
done. `orbit_task_list` has different defaults: it lists all statuses unless
filtered, with a default limit of 50. Do not equate an empty active search with
an empty task store.

Direct-server `workspaces` or `all_workspaces` can widen search when advertised
and authorized; managed runs cannot request all workspaces. Federated scope
uses that surface's returned host-qualified selectors. Inspect attribution on
each hit and mutate a result only through its owning workspace.
