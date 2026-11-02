# Operator attention

`orbit attention` / `orbit.attention` is a read-only derived view. It lists `pilot_warning`, `pilot_hold`, `blocked`, `decision_open`, `review_waiting`, `run_failed`, `run_stuck`, `claim_stale`, `host_unhealthy`, `notice_open` and `resident_stalled`. Reading it never dispatches or approves work. Open decisions fold into the hold, block or review they decide. `next` suggests a command and, when available, its exact tool/input; `needs` names arguments the operator must supply.

Scope defaults to the current workspace; `--workspace` repeats, and `--all` reads every active workspace. On a machine with registered hosts (`orbit host list`), `--all`, `--host`, `--workspace` or a call from outside any workspace reads every in-scope host in parallel within `attention.fanout_budget_seconds`; the federated MCP server does the same for a call without a host-qualified selector. Each host reports only what it is authoritative for: an owner its task-level kinds, every host its own health. Every item names its `host`, and federated items carry the workspace `selector`. A host that does not answer, answers under another machine id, or lacks the tool is a `host_unhealthy` item (`critical` when it owns an in-scope workspace) plus a `degraded` entry; `--exclude-kind host_unhealthy` hides the item, never the `degraded` entry. Provider kinds (`ci_red`, `friction_untriaged`) are later slices. A bound MCP session refuses `all`, `host` and other workspaces with `scope_not_permitted`. Agents can read task-level kinds; `run_failed`, `run_stuck`, `claim_stale` and `host_unhealthy` need the Operator capability and otherwise reports `degraded` code `capability`. Replica records are never substituted for their owner.

Runtime kinds read stored runs without reconciliation. `run_failed` reports recent failed,
timed-out or interrupted runs, suppressing older attempts superseded by a newer submission
of the same job/task and runs already named by a blocked item. `run_stuck` detects a gone
owner before reconciliation or a run beyond `attention.run_stuck_after_minutes`. Linux uses
`/proc` start identity with PID-namespace fencing; macOS uses libproc with a bounded `ps`
fallback. Unavailable identity is not proof of death; the age bound still applies. Active
runs remain visible beyond the failure window `attention.run_window_hours`.

`claim_stale` uses owner-side claim inspection and says “possibly stale”: reservation expiry,
a missing/terminal local bound run, unresolved merge intent or invalidated landing are
diagnostics. A remote run missing locally proves nothing. Its recovery advice uses
`orbit.task.recover_claim`, with `claim_id` and `workspace` known; `needs` declares `reason`,
`request_key`, `expected_phase` and `status` (blocked/backlog). Read `detail.phase` and inspect
the claim before supplying operator inputs. Attention never executes recovery, resume or
cancellation. Generic resume advice preserves the action's ordinary admission guards.

Local `host_unhealthy` reports failing or warning default `orbit doctor` workspace checks, which include a follower's pull drain ending on protocol skew. On a federated read it also reports a registered host whose binary version or pull-protocol fingerprint differs from this one.

`notice_open` reports one item per effective-open fleet notice in the read workspace; closed and expired notices never appear, and each item's `since` is the notice's opening time. It is default-selected with the task-level kinds and `host_unhealthy`, and it is derived only on the workspace owner: a replica reports a `degraded` entry with code `source_unavailable` instead of notice items. Severity starts from the notice kind (`incident` high, `maintenance` medium, `info` low) and steps up one level when the notice expires within two hours or its linked task sits terminal-but-unfinished (`rejected`, `archived`). `detail` carries `notice_kind`, the notice's own `scope` (`workspace` or `global`) and `expires_at`, and `reason` quotes the notice title. `next` suggests `orbit.notice.show`, with `orbit.notice.close` (needing `reason`) as the follow-up. A notice with a linked task attaches its `task_id` ref and `task` metadata; a taskless notice carries only its `notice_id` ref and no `task` object, so the task filters (`status`, `tag`, `task`, `crew`, `orchestrator`) exclude it.

`resident_stalled` reports an active resident mandate (not paused, taken over, handed off or ended) with no checkpoint for twice its heartbeat, or whose last three checkpoints are all `incomplete`. It is default-selected and derived only on the workspace owner. Severity starts at medium and follows the age-bump ladder; `detail` carries `stall` and `last_checkpoint_at`, and `since` is the last checkpoint (or the first of the three incomplete ones). `next` suggests `orbit.resident.control` `wake` with `status_request` (needs `reason`), with `orbit.resident.show` as the follow-up.

MCP advertises `orbit.attention` in `tasks-read`, included by the default surface and every role: operator, manager, resident, agent and reader. A role selects the tool surface; it grants no operator capability and never widens a bound workspace.

Filters are `workspace`, `all`, `host`, `kind`, `exclude_kind`, `severity`, `status`, `tag`, `task`, `crew`, `orchestrator`, `older_than`, `since`, `limit`. CLI flags use kebab-case. Repeated tool filters accept strings or arrays. Filters combine AND across fields, OR within a field; exclusions win. Task filters exclude taskless items. Durations accept `90m`, `36h`, `3d`. The public page defaults to 100, maximum 500.

```sh
orbit attention --kind pilot_warning --status proposed --json
orbit attention --all --severity high --json
orbit attention --host build-box --exclude-kind host_unhealthy --json
orbit attention --since '<cursor.next from the previous read>' --json
orbit tool run orbit.attention --input '{"kind":["pilot_hold","blocked"]}'
```

The envelope includes `items`, resolved `filters`, `counts`, `cursor`, `degraded`, an explicit `message` and, across hosts, `hosts` with each host's reachability. `degraded` means a source or host was unreadable or capped; empty is not all-clear. `candidate_read` is reserved for Orbit's own fan-out; never send it. NDJSON writes items followed by a summary record.

Retain `cursor.next` (opaque `atc1.` token) and pass it as `since`. New or materially changed items return; unchanged ones remain silent. Time alone leaves fingerprints unchanged except at a severity threshold. One cursor spans every host; while any source is degraded it keeps the hashes it already held, so a host that comes back does not repeat items already shown. Items omitted by `limit` remain unseen and return on a later poll. Resolved items disappear; there is no resolved-items feed. A cursor is stateless, contains hashes, grants no authority and expires after `attention.cursor_ttl_hours` (default 24). `cursor.status` is `none`, `accepted`, `expired`, `invalid` or `unsupported`; unusable cursors return the full list and a fresh token, never an error. Preserve an old token if a call fails. No session hook, schedule or notification is installed.

All shared settings have supported readback with `orbit config get attention.<key> --json`: `cursor_ttl_hours=24`, `age_bump_hours=[24,72]`, `run_window_hours=48`, `run_stuck_after_minutes=120`, `ci_cache_seconds=60`, `source_timeout_seconds=10`, `fanout_budget_seconds=20`, `per_workspace_item_cap=200`. Later sources consume the same typed table. Querying attention changes none of these settings.
