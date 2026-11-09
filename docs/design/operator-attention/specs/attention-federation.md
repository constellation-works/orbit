---
type: design
summary: "Spec: how orbit attention spans workspaces and hosts: authority per host, fan-out, unreachable hosts as items, and session binding"
last_validated: 2026-11-08
title: Spec — Attention federation
owner: claude
status: Draft
feature: operator-attention
tags: [operator-attention, federated-mcp, spec]
related_features: [operator-attention, federated-mcp, host-registry, distributed-drain, mcp-session-context]
related_artifacts: [ORB-21164, ORB-21171]
last_updated: 2026-11-08
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
   at most 200 items are kept, highest severity
   first, with `degraded` code `item_cap` when it cuts. The public response limit is applied
   once, after this merge, not once per workspace.
3. **All hosts.** The caller-side aggregator described next.

An unbound destination may borrow any active local checkout to open the attention
reader, including a replica when it owns no workspace. The checkout lends the
registry; each source still enforces the authority above. Attention does not
require a control-plane owner checkout merely to answer for that host.

## Fan-out across hosts

A federated caller (the CLI with registered hosts, or `orbit mcp serve --mode federated`) that
receives `orbit.attention` **without** a workspace selector:

1. Enumerates destinations: the implicit local one plus every host in `~/.orbit/hosts.toml`,
   narrowed by `--host`.
2. Issues one [candidate read](#candidate-read) per destination, in parallel. The hop carries
   `all: true` (or the resolved per-host `workspace` list when `--workspace` named workspaces;
   names and ids are resolved against the live federated workspace list, never guessed) and
   the item filters. It carries neither `since` nor `limit`. Destinations are stateless and
   unaware of the outer cursor.
3. Keeps a destination's items as its candidate set only when the [completeness](#completeness)
   rule says so. Merges those items, attaches `selector` to each item's `workspace` from the
   live list, concatenates `degraded`, builds `hosts[]`, then applies, in this order, ordering,
   `--since`, the public `--limit` and cursor minting
   ([attention-filters](./attention-filters.md#combination-semantics),
   [attention-cursor](./attention-cursor.md#delta-semantics)).

`host`, `all`, and a `workspace` argument are fleet scope, including a host-qualified
selector copied from a replica. The mux fans out to the destinations the `host` filter
keeps (every configured destination when `host` is omitted). A selector that encodes a
replica's machine still names that workspace's owner: the owner is in scope for the same
workspace id, and the hop carries that destination's local `ws_*`. An owner of a named
workspace who is not among those destinations is `degraded` with code `source_unavailable`
and scope `host:<name>`. The call must not come back empty with an empty `degraded` list
when that owner was not read. A workspace filter is echoed as the workspaces actually
opened; a destination that answers without echoing them does not wipe it.

A session already bound to one host-qualified workspace, whose call names neither `host`,
`all`, nor `workspace`, is still routed to that one destination as any workspace-scoped
call is. That destination is the outermost responder, so the public response limit applies
on it and the hop is not a candidate read. The call still returns an envelope.

Because the tool only reads, a lost answer after dispatch is safe to retry: it does not carry
the `outcome_unknown` ambiguity of a mutation. The aggregator still does not retry within one
call; it reports the loss.

### Candidate read

A candidate read is the aggregator's retrieval of one destination's post-filter items. It
is separate from the public response, whose omitted `limit` means 100 and whose maximum is
500 ([attention-filters](./attention-filters.md#public-response-limit)).

The hop is a `tools/call` of `orbit.attention` with these arguments:

```json
{
  "all": true,
  "candidate_read": true
}
```

The caller's item filters are included: `kind`, `exclude_kind`, `severity`, `status`, `tag`,
`task`, `crew`, `orchestrator` and `older_than`. `since` and `limit` are omitted. When the
outer call named workspaces, `workspace` replaces `all` and `candidate_read` stays `true`.

`candidate_read` is a reserved input, not a filter. The CLI has no flag for it, and the
public filter table does not list it. The MCP text tells agents and other user sessions not
to send it. The federation slice adds it to the tool schema so the mux can check the field
against the destination's advertised input before dispatch. A schema that does not advertise
`candidate_read` cannot take a candidate read. The aggregator then issues the public
arguments without `since` and without `limit`, and judges that body by the no-echo rule
in [Completeness](#completeness). A truncated public page becomes `response_truncated`.
It is not merged as the destination's full candidate set.

The destination returns every post-filter item it derived, bounded only by
the fixed cap of 200 per workspace, highest severity first.
Items the cap drops are absent from `items`. Each such workspace contributes `degraded`
code `item_cap`. On this read, `counts.matching` is the post-filter count before that cap,
`counts.returned` is the length of `items`, and `counts.truncated` is true only when the
cap dropped items. The public response limit is not applied: 150 matching items in one
workspace, under the cap, come back as 150 items. The envelope echoes `"candidate_read": true`
at the top level. This read does not apply the outer cursor. The aggregator discards any
`cursor` on the body.

The local destination does not cross a `tools/call`. The accepting process calls derivation
in candidate-read mode and receives the same envelope, including the echo. Remote hops use
the arguments above.

**Who may send `candidate_read`.** Only the aggregator, on a hop it originates. The CLI
cannot express it. A user MCP session — bound, unbound, or the outer federated call — that
includes `candidate_read` is the usage error `candidate_read_not_permitted` and returns no
items. The mux rejects the field on the outer call before fan-out, so a user's value is
never copied onto a hop. The mux sets `candidate_read: true` itself. The destination honors
the field only when the session's initialize `clientInfo.name` is `orbit-federated-mux`.
That name is self-reported, as other MCP client names are. The refusal keeps ordinary
clients on the public limit of 100. It is not an authentication check: an operator who can
open a session on the destination already owns that host and can already page the same items
with the public cursor. The mode changes the page size, up to the per-workspace cap, and
does not reveal a different set of items.

### Completeness

The aggregator merges `items` as the destination's whole candidate set only when all of
these hold:

- The body echoes `candidate_read: true`.
- `counts.truncated` is true exactly when the body contains at least one `item_cap` entry.
- When `counts.truncated` is false, `counts.matching` equals the number of items returned.
  When it is true, `counts.matching` is greater than the number returned, and the difference
  is the number of items the source cap dropped.

`item_cap` is the deliberate source cap. Those dropped items stay outside the merged set,
later polls do not grow them back, and the degraded entry remains on later calls that still
cut. Raising the outer `--limit` does not recover them. The aggregator does not also record
`response_truncated` for a cut that `item_cap` explains.

A body with no `candidate_read` echo is a public response. The aggregator may merge it as
the whole candidate set only when `counts.truncated` is false and `counts.matching` equals
the number of items returned: the public page held every item. Otherwise the public limit
cut the body (the default of 100, or any explicit public limit). The aggregator keeps the
items it was given, adds `degraded` code `response_truncated` at scope `host:<name>`, and
does not report the outer `counts.matching` as though the missing items were in the merged
set. The degraded message names the destination's reported matching and returned counts.

A body that echoes `candidate_read` but whose counts and `item_cap` entries disagree is
the same `response_truncated` failure. A partial body is never a complete candidate set.

Items missing from a partial body are not marked seen. A later hop that returns a complete
candidate read surfaces them once, because their hashes were never stored. Items already
returned stay in the cursor
([attention-cursor](./attention-cursor.md#delta-semantics), rules 3 and 4).
`response_truncated` is a failed read. `item_cap` is a configured bound. They are different
codes.

### Worked traces

Item numbers are the outer order after the merge: severity descending, then `since`
ascending, then `id`. Fingerprints do not change, and no host is unreachable. Each
candidate read below echoes `candidate_read: true`.

**150 items on one destination.** One workspace, 150 matching items, cap 200, so nothing
is cut. Every hop returns items 1..150, `counts.matching` 150, `counts.returned` 150,
`counts.truncated` false, and `degraded` empty. The outer `--limit` is the default 100.
`unchanged` follows rule 2.

| Poll | Outer `since` | Merged set | Returned | Cursor afterwards | `truncated` | `unchanged` |
|---|---|---|---|---|---|---|
| 1 | absent | 1..150 | 1..100 | hashes of 1..100 | true | 0 |
| 2 | poll 1 | 1..150 | 101..150 | hashes of 1..150 | false | 100 |
| 3 | poll 2 | 1..150 | empty | hashes of 1..150 | false | 150 |

Poll 1 returns 100 because the outer public limit cut the merged set. Rule 3 leaves
101..150 unseen, so poll 2 returns them. All 150 have been returned after poll 2. The
destination returned the same 150 on every hop; the outer cursor is what advances.

A destination that instead returned a public page of items 1..100, with `counts.matching`
150, `counts.returned` 100 and `counts.truncated` true, would not echo `candidate_read`.
The aggregator merges those 100, records `response_truncated`, and does not put 101..150
into the merged set. They stay absent until a later hop echoes a complete candidate read.
That earlier poll is partial, and the degraded entry says so. It is not a successful empty
delta.

**More than 500 aggregate items.** One destination, three workspaces, 200 items each, none
cut by the cap. The candidate read returns all 600, with `counts.truncated` false and no
`item_cap`. The outer caller passes `--limit 500`, the public maximum. The same outer
paging holds for any public limit from 1 to 500, and for the same 600 items split across
destinations, when every hop is a complete candidate read.

| Poll | Returned | Cursor afterwards | `truncated` | `unchanged` |
|---|---|---|---|---|
| 1 | 1..500 | hashes of 1..500 | true | 0 |
| 2 | 501..600 | hashes of 1..600 | false | 500 |

The outer `truncated: true` on poll 1 means the public page cut the merged set. It does
not mean a destination was partial. Rule 3 returns 501..600 on poll 2. Two destinations of
300 items each merge to the same set of 600 before that outer limit.

**Source cap.** One workspace, 250 post-filter items, cap 200. The candidate read returns
the 200 highest-severity items, `counts.matching` 250, `counts.returned` 200,
`counts.truncated` true, and one `item_cap` entry. The merged set is those 200. Later polls
do not return the other 50. The degraded entry is the record of the cut.

### Budgets

Each destination gets the federated probe budget for connect, handshake and discovery and the
call budget for the routed read, both bounded by a fixed 20 seconds as an outer deadline for the whole fan-out. A destination still running at the deadline is
treated as unreachable for this call. One slow host never delays the others past the deadline.

## Unreachable hosts are items

Every configured destination in scope that fails produces one `host_unhealthy` item, built by
the aggregator because the destination cannot report on itself:

| Fields | Value |
|---|---|
| `host` | the destination (`machine_id` from the pinned host entry, `name`) |
| `workspace` | `{id: null, name: null}` |
| `reason` | `<name> unreachable: <first line of the transport's error>`; for an input refusal, `<name> attention read refused: <first line of the error>` |
| `detail.cause` | `unreachable_destination` (no answer, timeout, identity mismatch with the pin), `tool_not_on_this_host` (reachable, but a binary without `orbit.attention`), or `attention_read_refused` (reachable, but the destination refused the read's input, such as a workspace selector refusal) |
| `since` | `null` (the aggregator keeps no history of onset) |
| `severity` | `critical` when the host is the owner of any in-scope workspace, as the local replica checkouts name it (`owner_machine_id`); otherwise `high` |
| `next` | `orbit host show <name>`; `also`: `orbit host list` for a live re-probe, and for `tool_not_on_this_host` the binary-update runbook |

Each such failure is *also* a `degraded` entry on the envelope (code `unreachable_destination`,
`tool_not_on_this_host`, or `attention_read_refused`, scope `host:<name>`). A destination
that refused the input still has `hosts[].reachability = reachable`; the refusal code
in the item and degraded entry distinguishes it from a transport failure.
The item is filterable (`--kind`,
`--exclude-kind host_unhealthy`), but `degraded` is not: an operator who excludes the kind
still sees that part of the fleet was blind, and the empty-result message stays `No items
found; N sources could not be read`, followed by the hosts and workspaces actually read.

A destination that answers but is under a different `machine_id` than its pin follows the same
path with `detail.cause = unreachable_destination` and a reason naming the mismatch; it is not
queried for items.

A destination that answers but whose binary version or pull-protocol fingerprint differs from
the accepting host's, as `orbit host list` reports skew, is still read. The aggregator adds one
`host_unhealthy` item for it with `detail.cause = version_skew`, severity as above, and no
`degraded` entry, because nothing was left unread.

### Local host health

Each destination reports its own health as part of its candidate read, once per in-scope
workspace checkout: one `host_unhealthy` item when any default `orbit doctor` workspace check
fails or warns, with `detail.cause = doctor` and the failing checks in `detail.checks` (errors
first, at most 32). Those checks include the follower's latest pull drain, so an owner refusing
its `orbit.drain.probe` on protocol skew appears here. Severity is `critical` when the answering
host owns the workspace, otherwise `high`; `since` is `null` because the doctor records no
onset, and the fingerprint covers each failing check's name and status, not its message. A
session without the Operator capability gets a `degraded` entry with code `capability` instead.

### CLI routing

On a machine with registered hosts, `orbit attention` takes the aggregator path when the call
names a scope (`--all`, `--host`, `--workspace`) or runs outside any workspace. A bare call inside
a workspace checkout, a call with `--root`, and a machine with no registered hosts read locally.
The rendered table adds a `HOST` column when the envelope carries `hosts`.

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
visible. Each destination hop is a [candidate read](#candidate-read): the mux sets
`candidate_read` and does not forward a value the user supplied. v1 has exactly one such
tool, `orbit.attention`. This is not store replication, synchronization or silent merging:
nothing is written, nothing is cached, and no item is presented without its host. Slice 4
edits the federated-mcp design and spec to say so, including this hop
([4_decisions.md](../4_decisions.md#fan-out-is-a-named-exception-in-the-federated-mux)).

[ORB-21164] specified federation; [ORB-21182] specified its untruncated candidate read.

## Task References

- [ORB-21164] — specified the federated behaviour of attention
- [ORB-21182] — specified the candidate read that separates destination retrieval from the public response limit

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
