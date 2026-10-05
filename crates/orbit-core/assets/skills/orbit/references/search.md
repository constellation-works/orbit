# Search

`orbit search` and `orbit.search` retrieve tasks and frictions using lexical
matching. For source callers, symbols, and implementations, read repository
files or use `rg`. Include `model` and the returned workspace selector when
calling task tools.

```bash
orbit search "scheduler retry" --kind task --limit 5
orbit search "scheduler" --tag perf --kind all
orbit search "recovery" --all
orbit tool run orbit.search --input '{"query":"scheduler retry","kind":"task","limit":5,"model":"<agent-family>","workspace":"<workspace-id>"}'
```

Task search ranks indexed title, description, acceptance criteria, plan, and
execution summary chunks with SQLite FTS5 BM25. Query words need not be adjacent:
`"scheduler retry"` requires both terms in a chunk, not the exact phrase. Terms
are quoted literally before FTS parsing. Prefer a few distinctive terms from
the task; search does not infer synonyms. Task create/update writes chunks
synchronously in both CLI and long-lived hosts; deletion retracts them.

The bundle matcher supplements BM25 for comments, external references, artifact
manifest paths, and unindexed tasks. It matches a case-insensitive substring,
so a multi-word query on these fields must be contiguous. Artifact payloads are
not searched. Frictions use their existing lexical matcher.

Before creating a task, search its distinctive title or description terms to
check for duplicates. Use the same query form for prior context after loading a
task. When useful results appear, inspect them before trying another query.

Results retain `mode: "lexical"`, `kind`, `notes`, and `results`. Read the task
record to judge relevance. `--kind` accepts `task`, `friction`, or `all`.
Repeated `--tag` values use AND. `--status` takes `kind:value` tokens, such as
`task:open`; explicit statuses override `--all` for that kind.

Ordinary searches hide closed history. `all: true` includes normally hidden
statuses; use a bounded all-status pass before concluding a repair was never
done. Task listing has different defaults and is not an equivalent search.

`workspaces` or `all_workspaces` can widen search when advertised and authorized.
Managed runs may only search their own workspace. Federated hits identify their
workspace; follow up through the owning workspace. Per-workspace results are
interleaved by rank.

For imports or restored task bundles, an operator can rebuild the index:

```bash
orbit search reindex
orbit doctor
```

Reindex replaces task chunks from the task store and reports task/chunk counts.
The `search-index` doctor check reports indexed tasks versus stored tasks.
Search requires no model download or separate process. The database keeps its
legacy `semantic.db` filename; see the upgrade runbook for migration details.
