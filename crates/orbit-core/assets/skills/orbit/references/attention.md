# Operator attention

`orbit attention` / `orbit.attention` is a read-only derived view. It lists `pilot_warning`, `pilot_hold`, `blocked`, `decision_open` and `review_waiting`. Reading it never dispatches or approves work. Open decisions fold into the hold, block or review they decide. `next` suggests a command and, when available, its exact tool/input; `needs` names arguments the operator must supply.

Scope defaults to the current workspace; `--workspace` repeats, and `--all` reads the local registry. Cross-host fan-out and runtime/provider kinds are later slices. A bound MCP session refuses `all`, `host` and other workspaces with `scope_not_permitted`. Agents can read task-level kinds. Replica records are never substituted for their owner.

Filters are `workspace`, `all`, `host`, `kind`, `exclude_kind`, `severity`, `status`, `tag`, `task`, `crew`, `orchestrator`, `older_than`, `since`, `limit`. CLI flags use kebab-case. Repeated tool filters accept strings or arrays. Filters combine AND across fields, OR within a field; exclusions win. Task filters exclude taskless items. Durations accept `90m`, `36h`, `3d`. The public page defaults to 100, maximum 500.

```sh
orbit attention --kind pilot_warning --status proposed --json
orbit attention --all --severity high --json
orbit attention --since '<cursor.next from the previous read>' --json
orbit tool run orbit.attention --input '{"kind":["pilot_hold","blocked"]}'
```

The envelope includes `items`, resolved `filters`, `counts`, `cursor`, `degraded` and an explicit `message`. `degraded` means a source was unreadable or capped; empty is not all-clear. NDJSON writes items followed by a summary record.

Retain `cursor.next` (opaque `atc1.` token) and pass it as `since`. New or materially changed items return; unchanged ones remain silent. Time alone leaves fingerprints unchanged except at a severity threshold. Items omitted by `limit` remain unseen and return on a later poll. Resolved items disappear; there is no resolved-items feed. A cursor is stateless, contains hashes, grants no authority and expires after `attention.cursor_ttl_hours` (default 24). `cursor.status` is `none`, `accepted`, `expired`, `invalid` or `unsupported`; unusable cursors return the full list and a fresh token, never an error. Preserve an old token if a call fails. No session hook, schedule or notification is installed.

All shared settings have supported readback with `orbit config get attention.<key> --json`: `cursor_ttl_hours=24`, `age_bump_hours=[24,72]`, `run_window_hours=48`, `run_stuck_after_minutes=120`, `ci_cache_seconds=60`, `source_timeout_seconds=10`, `fanout_budget_seconds=20`, `per_workspace_item_cap=200`. Later sources consume the same typed table. Querying attention changes none of these settings.
