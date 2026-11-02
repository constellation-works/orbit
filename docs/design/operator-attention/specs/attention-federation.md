---
type: design
summary: "Spec: how orbit attention spans workspaces and hosts: authority per host, fan-out, unreachable hosts as items, and session binding"
last_validated: 2026-11-01
title: Spec — Attention federation
owner: claude
status: Draft
feature: operator-attention
tags: [operator-attention, federated-mcp, spec]
related_features: [operator-attention, federated-mcp, host-registry, distributed-drain, mcp-session-context]
related_artifacts: [ORB-21164]
last_updated: 2026-11-01
---

# Spec: Attention federation

One `orbit attention` call from any host covers every workspace that host reaches: its own
checkouts and the workspaces on hosts registered with `orbit host add`. Each host reports only
what it is authoritative for, a host that cannot be reached becomes an item rather than a
silent gap, and the merged list is cursor-filtered once, at the outermost caller.

## Why This Exists

A follower (the Mac, or dk-server-1) holds replica task records that go stale, and the owner
(dk-server-2) holds the coordination store. If the follower derived task-level items from its
replica it would report stale truth; if it ignored the owner it would miss the pilot warnings
that motivated this feature. If an unreachable owner simply yielded an empty list, the
operator would read "nothing needs attention" at the moment nothing can be seen.

## Authority per host

The tool is **unclassified** in the federated capability scheme: like workspace discovery, it
is never refused with `capability_refused`. Instead the destination derives only the kinds it
is authoritative for, from its catalog role for each workspace
([federated-mcp 2_design.md §3–4](../../federated-mcp/2_design.md)):

| Checkout role on the answering host | Kinds derived |
|---|---|
| **Owner** (`owner_machine_id` is this machine) | all task-level kinds, `claim_stale`, `ci_red`, `friction_untriaged`, its own `run_failed` / `run_stuck`, its own `host_unhealthy` |
| **Replica** | its own `run_failed` / `run_stuck` and its own `host_unhealthy` only |

Task, decision, review, claim and friction records on a replica are not authoritative, so a
replica never reports them, and no caller substitutes a replica's copy for an unreachable
owner. This matches the rule that Orbit does not fail over to a cached or substitute route
([federated-mcp 2_design.md §6](../../federated-mcp/2_design.md)). Runs are authoritative on the
host that executes them, so a follower reports its own leaf runs; when that run fails and
settles to the owner, the owner's `blocked` item carries the task-level consequence.

By construction no two hosts report the same condition, so merging needs no de-duplication
beyond the folding rule inside one host.

## Three scopes

1. **One workspace, one host.** The core tool with `workspace` set (or the session binding)
   derives that workspace's items from the answering host's checkout. This is slice 1.
2. **All workspaces on one host.** `all: true` makes the accepting server's local aggregator
   iterate its registry's active workspaces, derive each, and merge. A workspace whose
   derivation fails contributes `degraded` entries and does not fail the call. Per workspace,
   at most `attention.per_workspace_item_cap` items (default 200) are kept, highest severity
   first, with `degraded` code `item_cap` when it cuts.
3. **All hosts.** The caller-side aggregator described next.

## Fan-out across hosts

A federated caller (the CLI with registered hosts, or `orbit mcp serve --mode federated`) that
receives `orbit.attention` **without** a workspace selector:

1. Enumerates destinations: the implicit local one plus every host in `~/.orbit/hosts.toml`,
   narrowed by `--host`.
2. Issues one `orbit.attention` call per destination, in parallel, with `all: true` (or the
   resolved per-host `workspace` list when `--workspace` named workspaces; names and ids are
   resolved against the live federated workspace list, never guessed) and the item filters,
   but **without** `since` and `limit`. Destinations are stateless and unaware of the cursor.
3. Merges the item lists, attaches `selector` to each item's `workspace` from the live list,
   concatenates `degraded`, builds `hosts[]`, then applies, in this order, ordering, `--since`,
   `--limit` and cursor minting ([attention-filters](./attention-filters.md#combination-semantics)).

A call that carries a host-qualified selector is routed to that one destination as any
workspace-scoped federated call is, and still returns an envelope.

Because the tool only reads, a lost answer after dispatch is safe to retry: it does not carry
the `outcome_unknown` ambiguity of a mutation. The aggregator still does not retry within one
call; it reports the loss.

### Budgets

Each destination gets the federated probe budget for connect, handshake and discovery and the
call budget for the routed read, both bounded by `attention.fanout_budget_seconds` (default
20) as an outer deadline for the whole fan-out. A destination still running at the deadline is
treated as unreachable for this call. One slow host never delays the others past the deadline.

## Unreachable hosts are items

Every configured destination in scope that fails produces one `host_unhealthy` item, built by
the aggregator because the destination cannot report on itself:

| Fields | Value |
|---|---|
| `host` | the destination (`machine_id` from the pinned host entry, `name`) |
| `workspace` | `{id: null, name: null}` |
| `reason` | `<name> unreachable: <first line of the transport's error>` |
| `detail.cause` | the federated routing identity: `unreachable_destination` (no answer, timeout, identity mismatch with the pin) or `tool_not_on_this_host` (reachable, but a binary without `orbit.attention`) |
| `since` | `null` (the aggregator keeps no history of onset) |
| `severity` | `critical` when the host is the owner of any in-scope workspace, as the local replica checkouts name it (`owner_machine_id`); otherwise `high` |
| `next` | `orbit host show <name>`; `also`: `orbit host list` for a live re-probe, and for `tool_not_on_this_host` the binary-update runbook |

Each such failure is *also* a `degraded` entry on the envelope (code `unreachable_destination`
or `tool_not_on_this_host`, scope `host:<name>`). The item is filterable (`--kind`,
`--exclude-kind host_unhealthy`), but `degraded` is not: an operator who excludes the kind
still sees that part of the fleet was blind, and the empty-result message stays `No items
found; N sources could not be read`.

A destination that answers but is under a different `machine_id` than its pin follows the same
path with `detail.cause = unreachable_destination` and a reason naming the mismatch; it is not
queried for items.

## Cursor and recovery across hosts

Item ids contain `host.machine_id` and `workspace.id`, so one cursor spans every host. A host
that was unreachable last call and answers now brings its items back; because `degraded` was
non-empty on the earlier call, the cursor carried forward the hashes it already held
([attention-cursor](./attention-cursor.md#delta-semantics), rule 4), so recovery does not
re-inject items the caller had seen. The `host_unhealthy` item itself leaves the list when the
host answers.

## Session binding and capability

- A v1 MCP session bound to a workspace (`orbit mcp serve --workspace …`) answers only that
  workspace. `all`, other workspaces and `--host` are refused with `scope_not_permitted`; a
  bound session never widens its own scope, and a binding is not a default the tool can
  override.
- An unbound local session and a federated session may use `all` and fan out. Whether a host
  answers its runtime kinds depends on the destination session's capability: a destination
  session holding only `agent` reports task-level kinds and a `degraded` entry
  (`capability`) for the runtime kinds it may not read.
- The CLI (`orbit attention`) on an operator's shell runs with local Operator capability and
  reaches remote destinations over the same SSH sessions the federated mux uses, with the
  capability that session holds.

## Amendment to the federated-mcp contract

Today a workspace-scoped federated call needs a host-qualified selector, and a bare call is
`unknown_selector`. This feature adds one narrow exception: a tool the contract names as a
**fan-out read** may be called without a selector. The mux then calls every in-scope
destination and merges explicitly, with each item attributed to its host and every failure
visible. v1 has exactly one such tool, `orbit.attention`. This is not store replication,
synchronization or silent merging: nothing is written, nothing is cached, and no item is
presented without its host. Slice 4 edits the federated-mcp design and spec to say so
([4_decisions.md](../4_decisions.md#fan-out-is-a-named-exception-in-the-federated-mux)).

## Task References

- [ORB-21164] — specified the federated behaviour of attention

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
