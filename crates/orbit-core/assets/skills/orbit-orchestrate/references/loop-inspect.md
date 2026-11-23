# Inspect a workspace before preparing work

## Establish authority and inspect

Call the connected `orbit_workspace_list`, select the owning host/workspace,
and reuse its exact returned selector in every durable tool call. Prefer MCP;
CLI examples here assume an authorized administrative surface on that same
host with workspace routing already established. A checkout path alone does
not establish authority. See [tool-surface-routing.md](../../orbit/references/tool-surface-routing.md).

Read workspace goals, existing tasks, recent runs, and the relevant repository
or runtime evidence. Use `orbit_task_list` with its advertised filters and a
`fields` projection (full records are large), and `orbit_task_show` with field
projections. For run diagnostics:

```bash
orbit run readiness --json
orbit run history --limit 20 --json
orbit run show <run-id> --json
```

CLI `orbit run readiness`, MCP `orbit_workflow` action `auto` with `mode: "status"`,
and HTTP `GET /api/workflows/auto/readiness` render the same readiness snapshot.
For MCP status, `task` accepts one task id or a list, `concurrency` previews a
ceiling, `limit` defaults to 50 (1–500), and `allow_crew` selects configured
crews. HTTP accepts comma-separated `task` and `allow_crew` query values plus
`concurrency` and `limit`. Omitted crews use the live drain's restriction when
present.

All three JSON responses include `replica` and
`validation_env_warning` (null when absent); CLI text prints a warning when set.

Readiness is a snapshot, not a reservation or promise of dispatch. Request
bounded lists and project the needed fields; full run bundles can contain
large provider logs. Keep those logs for targeted failure diagnosis.
