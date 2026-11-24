# Operator attention: reading, filters and cursor

## What attention lists

`orbit attention` / `orbit.attention` is a read-only derived view. It lists `pilot_warning`, `pilot_hold`, `blocked`, `decision_open`, `review_waiting`, `run_failed`, `run_stuck`, `claim_stale`, `ci_red`, `friction_untriaged`, `host_unhealthy`, `notice_open`, `resident_stalled`, `routine_tripped`, `drain_paused` and `drain_breaker_open`.

Reading it never dispatches or approves work. Open decisions fold into the hold, block or review they decide. `next` suggests a command and, when available, its exact tool/input; `needs` names arguments the operator must supply.

## Kind filters and scope

Kind filters select sources before any reads. Reading attention starts no session hooks or background work, and suggested recovery or cancellation commands grant no authority.

Scope defaults to the current workspace; `--workspace` repeats, and `--all` reads every active workspace. On a machine with registered hosts (`orbit host list`), `--all`, `--host`, `--workspace` or a call from outside any workspace reads every in-scope host in parallel within a 20-second budget; the federated MCP server fans out for `host`, `all`, or a workspace, including a host-qualified selector.

A session bound to one host-qualified workspace stays on that checkout only when the call names none of those. Each host reports only what it is authoritative for: an owner its task-level kinds, every host its own health. Every item names its `host`, and federated items carry the workspace `selector`.

A host that does not answer, answers under another machine id, or lacks the tool is a `host_unhealthy` item (`critical` when it owns an in-scope workspace) plus a `degraded` entry; `--exclude-kind host_unhealthy` hides the item, never the `degraded` entry. A bound MCP session refuses `all`, `host` and other workspaces with `scope_not_permitted`.

An active `orbit host offline` mark lists the host once under `acknowledged`, with the operator's reason and any deadline. Attention skips probing that host. Treat the entry as planned downtime until the deadline or until the operator runs `orbit host online`.

Agents can read task-level kinds; `run_failed`, `run_stuck`, `claim_stale` and `host_unhealthy` need the Operator capability and otherwise report `degraded` code `capability`. A live resident cycle also reads them, read-only, for the workspaces its mandate covers and no others (host health is withheld while any selected workspace is outside the mandate); this gives it no write capability.

While its home workspace has a pending task commit, the cycle cannot verify its scope and these runtime kinds report `capability` degradation until an ordinary task read recovers the commit. Attention leaves the pending commit untouched. Replica records are never substituted for their owner.

## Tool surface and filters

MCP advertises `orbit.attention` in `tasks-read`, included by the default surface and every role: operator, manager, resident, agent and reader. A role selects the tool surface; it grants no operator capability and never widens a bound workspace.

Filters are `workspace`, `all`, `host`, `kind`, `exclude_kind`, `severity`, `status`, `tag`, `task`, `crew`, `orchestrator`, `older_than`, `since`, `limit`. CLI flags use kebab-case. Repeated tool filters accept strings or arrays. Filters combine AND across fields, OR within a field; exclusions win. Task filters exclude taskless items. Durations accept `90m`, `36h`, `3d`. The public page defaults to 100, maximum 500.

```sh
orbit attention --kind pilot_warning --status proposed --json
orbit attention --all --severity high --json
orbit attention --host build-box --exclude-kind host_unhealthy --json
orbit attention --since '<cursor.next from the previous read>' --json
orbit tool run orbit.attention --input '{"kind":["pilot_hold","blocked"]}'
```

## Response envelope

The envelope includes `items`, resolved `filters`, `counts`, `cursor`, `degraded`, an explicit `message` and, across hosts, `hosts` with each host's reachability. A workspace filter is echoed after it is resolved. An empty `message` names the hosts and workspaces actually read.

`degraded` means a source or host was unreadable or capped; empty is not all-clear. An owner of a named workspace who was not among the hosts read is `degraded`, not an empty all-clear. `candidate_read` is reserved for Orbit's own fan-out; never send it. NDJSON writes items followed by a summary record.

## Delta cursor

Retain `cursor.next` (opaque `atc1.` token) and pass it as `since`. New or materially changed items return; unchanged ones remain silent. Time alone leaves fingerprints unchanged except at a severity threshold. One cursor spans every host; while any source is degraded it keeps the hashes it already held, so a host that comes back does not repeat items already shown. Items omitted by `limit` remain unseen and return on a later poll.

Resolved items disappear; there is no resolved-items feed. A cursor is stateless, contains hashes, grants no authority and expires after 24 hours. `cursor.status` is `none`, `accepted`, `expired`, `invalid` or `unsupported`; unusable cursors return the full list and a fresh token, never an error. Preserve an old token if a call fails. No session hook, schedule or notification is installed.

## Waiting without a model turn

To wait without spending a model turn, mint a grant on a read and block in the shell. Call `orbit.attention` with `wait_grant: true` over MCP, then run `ORBIT_ATTENTION_WAIT_TOKEN=<wait_grant.token> orbit attention wait --since '<cursor.next>' --workspace <ws> --timeout 30m`.

The grant lasts two hours, covers only the workspaces that read named, and gives agent-level reads (operator kinds stay `degraded`); a shell without one is refused with `capability_denied`, and so is a plain `orbit attention` that names `--workspace`, `--host` or `--all`: a selector is not a capability.

Exit `0`: new items, printed with the next cursor. Exit `3`: timeout with no change. Exit `4`: the `degraded` set changed. Exit `5`: the final read before the deadline failed, either every read failed or only the last one after earlier reads succeeded. No envelope is printed and the `wait` record carries the error, so exit `5` alone does not show that every poll failed. A timeout still lists `degraded`, and `wait.all_sources_readable` is false while any source is unreadable: not all-clear. Re-mint when the grant expires.

## Fixed values

These values are fixed in code, not settings: `cursor_ttl_hours=24`, `age_bump_hours=[24,72]`, `run_window_hours=48`, `run_stuck_after_minutes=120`, `ci_cache_seconds=60`, `source_timeout_seconds=10`, `fanout_budget_seconds=20`, `per_workspace_item_cap=200`.

The former `attention.*` keys are removed: they load only as ignored keys (recognised until 2026-12-08), `orbit config get`/`set` refuse them, and there is no config readback for them; delete them from config. Querying attention changes none of these fixed values.
