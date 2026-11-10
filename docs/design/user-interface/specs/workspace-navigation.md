---
type: design
summary: "Spec: Workspace Navigation Across Hosts"
tags: ["user-interface", "host-registry", "distributed-drain"]
last_validated: 2026-11-09
---

# Spec: Workspace Navigation Across Hosts

> **Partly implemented.** The host × All workspaces drain controls in §4 reuse
> the existing dashboard, and the rail's workspace dropdown now offers only the
> workspaces the selected host owns (`checkout_role` on each `/api/workspaces`
> row; no role counts as owned), so a follower's replica checkout has no board
> on any host. Catalog, scope, host facets and host navigation below
> remain proposed; [2_design.md](../2_design.md) describes the dashboard as it is.

The dashboard navigates by logical workspace, never by host and workspace together. Each
workspace appears once, its requests go to the host that owns it, and the hosts that hold a
replica checkout of it remain selectable in the All workspaces Auto-drain card.
One scope, a set of workspaces plus the time window, applies to every view.

## Why This Exists

The host picker ([ORB-14680]) moves the whole dashboard to another host, and each host lists
every workspace it has a checkout of. An operator with 3 hosts and 12 workspaces therefore has
36 boards. Most of them are follower boards, and a follower board has one use: starting or
stopping a pull drain. Tasks, dependencies, comments and claim state are owner-read and
owner-write with no replica-local fallback
([distributed-drain §5](../../distributed-drain/2_design.md#5-transport-and-authority-routing)),
so a follower board shows nothing the owner's board lacks.

The second cost is that only some views accept **All workspaces**. An operator who wants "the
jobs of these four workspaces" has to visit four boards.

## Terms

| Term | Meaning |
|---|---|
| Serving host | The host whose `orbit web serve` the browser talks to. |
| Owner | The host whose checkout of a workspace has the owner role ([host-registry](../../host-registry/2_design.md#owner-and-replica-roles)). |
| Capacity host | A host with a replica checkout of the workspace. It can run a pull drain for it. |
| Catalog | The browser's list of logical workspaces across all registered hosts. |
| Scope | The selected set of workspaces plus the time window. |

## 1. Workspace catalog

1. The browser builds the catalog from `GET /api/workspaces` on the serving host and
   `GET /api/on/<host>/workspaces` on each registered remote, with at most three requests in
   flight. This is the fan-out the fleet drain card already uses.
2. Each `/api/workspaces` row carries `checkout_role` and `owner_machine_id`. The dashboard's
   workspace binding already holds both; the list response must expose them.
3. Rows are merged by logical workspace ID. A catalog entry holds the workspace ID and name,
   the owner host, and the capacity hosts.
4. The workspace picker lists each catalog entry once, grouped under its owner's name. It is
   the checkbox listbox the dashboard ships today, a multi-select, not a native select. Each
   owner's entries sit in a `role="group"` labelled with the owner's name, and a non-selectable
   label row shows that name above them; the label row is not a `role="option"`, so it cannot
   be ticked. The shipped listbox is flat, so the owner rows arrive with the catalog. The host
   picker is the one that stays a native select. A replica checkout never produces its own
   entry.
5. A workspace whose owner is not registered on the serving host, or does not answer, stays in
   the picker marked **owner unreachable**. Its views show that state. The dashboard does not
   read the replica's local store in its place.
6. While a host's list is still loading, entries already known stay usable and the picker
   marks the catalog as partial. A host that fails is named in the picker.
7. An installation without host identity has one host and no roles. Its catalog is its own
   workspace list, and nothing else in this spec changes its behaviour.

## 2. Owner routing

1. Every request for a workspace goes to that workspace's owner: `/api/<rest>` when the
   serving host owns it, `/api/on/<owner>/<rest>` otherwise. The one API path chokepoint
   resolves the host from the workspace, not from a selected host.
2. The owner is read from the catalog on each catalog refresh. After an
   [owner failover](../../owner-failover/2_design.md) the next refresh routes to the new
   owner; a request that reaches a host which is no longer the owner fails with that host's
   typed refusal and triggers a catalog refresh.
3. A mutation is always sent with an explicit `workspace` and to that workspace's owner.
   There is no ambient host.
4. `?host=` no longer selects a board. A link carrying `?host=H&workspace=W` normalizes in
   place: to `?workspace=W` when H owns W, and to All workspaces with the Drain dock
   selecting W and H when H is a capacity host of W. The remembered host is discarded.
5. The host registry routes (`/api/hosts…`) stay on the serving host, as today.

## 3. Scope

1. `?workspace=` accepts one workspace ID, a comma-separated list of IDs, or `all`. With
   `?window=` it is the source of truth for every view. An ID the catalog does not hold is
   dropped from the list; an empty result falls back as a bare URL does.
2. The picker is a multi-select over the catalog, grouped by owner. Choosing one entry gives
   a single-workspace scope, which behaves as a selected workspace does today.
3. The operator can save the current selection under a name. Saved scopes are stored in the
   browser and listed at the top of the picker. A saved scope that names a workspace the
   catalog no longer holds keeps its other members.
4. Every view reads the scope. Views fall into two classes:

   | Class | Views | With more than one workspace in scope |
   |---|---|---|
   | List views | Tasks, Jobs (queue and History), Leases, Automation, Health › Incidents and Events, Knowledge | Rows from every workspace in scope, each with a workspace chip, and a workspace filter in the view's filter row. |
   | Single-workspace views | Analytics › Codebase, Resources, Forecast, Experiments and Agents; Health › Doctor; Plugins; Settings | A workspace selector in the view's header, limited to the scope's members. The view remembers its choice for that scope. This replaces the aggregate placeholder. |

5. A list view asks each owner once: the scope's workspaces are grouped by owner, and each
   owner's aggregate endpoint receives its group as a `workspace=a,b` filter. Aggregate
   endpoints accept that filter in addition to today's all-or-one.
6. Results from owners that have answered are shown while others are marked `loading`. An
   owner that fails is named in a banner on the view, and its rows are absent, not stale.
   A partial result is never cached as a complete one.
7. Rows from several owners are merged in the browser by the view's sort key. A paged view
   keeps one cursor per owner and requests the next page from each owner that has more.
8. A row action is sent to the row's own `workspace_id` and that workspace's owner. This is
   the rule the aggregate Tasks view already follows ([ORB-10874], [ORB-12235]), applied to
   every list view.
9. An action that needs exactly one workspace (New task, Start drain, a configuration edit)
   uses the scope's workspace when the scope has one. Otherwise the control asks for a
   workspace from the scope's members before it enables its submit button. It never guesses.

## 4. Drain windows under All workspaces

The host × **All workspaces** view reuses the existing Auto-drain card's
workspace and host selects, **Start a window** card and **Stop** control. It
adds no Hosts list, lane controls, fields, markup or styles. A concrete
host/workspace board, including a replica board, stays as it is.

1. The Auto-drain card's workspace select is limited to the rail's selected
   workspaces. Its host select lists the workspace's owner first, then the
   followers holding a replica. A host whose fleet read failed stays in the
   select, and when it is the chosen host the card shows that read's error.
   Both selections survive reloads; changing workspace resets the host to its
   owner.
2. The card reads `GET /api/on/<host>/workflows/auto/readiness?workspace=<id>`
   for the chosen pair, including when Idle. It shows that host's window,
   capacity, checkout and start policy rather than an aggregate of the hosts.
3. **Start a window** keeps the concrete board's fields and confirmation.
   An owner start posts its usual body to `/api/workflows/auto` for the serving
   owner, or `/api/on/<owner>/workflows/auto` otherwise, explicitly qualified
   by workspace. A follower's card says **Start pulling on <host>** and posts
   `{pull: <workspace>, host: <owner>, for_duration: <duration>}` plus any
   explicit concurrency to `/api/on/<follower>/workflows/auto`.
4. The existing **Stop** posts to `workflows/auto/stop` on the same host for
   the same workspace, with the existing owner or pull-drain confirmation.
   **Send pending results** stays on the replica it applies to. Start and Stop
   refresh readiness from that selected host; changing the selection retires
   earlier actions' readbacks.
5. A host that refuses forwarded writes disables Start and Stop with
   `hostWriteRefusal`'s existing wording. A follower missing the replica or
   owner routing disables Start with the replica board's existing
   unavailable-owner message. No refused start is posted.
6. Existing run links address the chosen host and workspace. The separate
   **Fleet** tab retains its per-host tabs and rows, which switch host and
   workspace as today.

## 5. Host as a facet

1. Runs, leases, worktrees and log tails name the host they belong to. Jobs and Leases have a
   host filter beside the workspace filter.
2. The run-detail route carries the executing host, so a link to a follower's run resolves
   through the forward without changing the scope.
3. The rail's drain reading counts live windows across the scope (`2 live`), on every view.

## 6. Hosts page

1. The host picker is removed from the rail. **Hosts** becomes a page under Manage and takes
   over what Settings › Hosts does today.
2. The page has one row per registered host: reachability, binary version and protocol
   fingerprint, resource readings, provider usage, the drains it is running, and the
   workspaces it owns and replicates.
3. A host's detail holds what is true of the host and not of a workspace: its Doctor report,
   installed plugins, scheduler state and host file.
4. Health › Doctor keeps a host selector in its own header, defaulting to the owner of the
   view's workspace, because a doctor report is for one workspace on one host.

## Phasing

The first delivered step is deliberately smaller than the catalog phase below:
on one host, the rail workspace dropdown becomes a persisted, URL-addressed
checkbox multi-select (one tick keeps the existing board; multiple ticks narrow
All workspaces task rows and counts). All workspaces gains an owner-first
Auto-drain host select and a separate Fleet dock tab. Its existing Start a
window card and Stop control then start and stop the chosen pair, including
follower pull drains, before replica entries leave the workspace dropdown.
This step adds no All hosts entry, catalog or owner routing, and does not
collapse follower workspaces. It precedes the original catalog phase 1; the
revised phase 2 below describes the drain controls that build on it.
Replica entries have since left the workspace dropdown: it offers only the
selected host's owned workspaces, the All workspaces aggregate and the
Auto-drain card's workspace select follow that owned list (its host select
still lists follower hosts), and a remembered or linked replica falls back
to the host's owned workspaces.

| Phase | Change | Removes |
|---|---|---|
| 1 | Catalog and owner routing (§1, §2 rules 1 to 3 and 5). The host picker gains **All hosts** as its first and default entry, which shows the catalog. Choosing one host still shows that host's own board, as today, so a follower's pull drain stays reachable. | Duplicate workspace entries in the default view. |
| 2 | Reuse Start a window and Stop for the workspace/host selected in the All workspaces Auto-drain card (§4), including follower pull drains. No new controls, styles or mocks; concrete boards stay unchanged. Replica entries can then leave the workspace dropdown. The host facet (§5) and `?host=` normalization (§2 rule 4) remain later work. | The need to open a follower board to start or stop a pull drain. |
| 3 | Multi-workspace scope in every view (§3). | The aggregate placeholder. |
| 4 | Hosts page; the host picker is removed (§6). | Host as a navigation level. |

Phases 1 and 2 end the 36-board problem. Phase 3 is the largest: it changes each aggregate
endpoint and each list view.

## Mocks

The earlier phase-2 Hosts-list design and its mocks are dropped. The drain
window change in §4 reuses the current Start a window card and Auto-drain Stop
control; it needs no mocks or new CSS. The historical files under
[`../references/mocks/`](../references/mocks/) are not implementation direction
for this phase. Catalog and host-facet work remains proposed.

The catalog mocks remain references for future phase 1:

| Mock | Component | Change | New CSS |
|---|---|---|---|
| [1.1](../references/mocks/1-1-pickers.html) | Host select and workspace listbox in the rail | Host gains All hosts; the workspace listbox lists each workspace once under owner group labels; unreachable and loading entries | None |
| [1.2](../references/mocks/1-2-owner-unreachable.html) | Tasks list empty state | The replica empty state, reworded for an unreachable owner | None |

The host column and filter on Jobs and Leases (§5) are not mocked.

## Concerns and Honest Limitations

- **A scope spanning owners costs one request per owner per view.** With the three-request
  limit and a slow host, a list view fills in visibly later for that owner's rows.
- **Merged paging is approximate.** Until every owner has returned its page, the merged order
  can change as rows arrive.
- **The serving host must reach the owner.** A serving host that can reach only a follower
  shows that workspace as owner unreachable, where today it could show the follower's board
  and its pull drain controls. A reachable follower remains selectable for a
  pull drain under All workspaces.
- **Saved scopes are per browser.** They do not follow the operator to another machine.

## Open Questions

1. Which follower-run facts does the owner already record? The drain card's run link and the host
   facet need the executing host on run and lease rows. If the owner holds only the claim,
   Jobs has to merge the capacity hosts' run lists into the owner's.
2. Should saved scopes live in the serving host's configuration so they are shared across
   browsers?
3. Do the Analytics views that are single-workspace today have a meaningful multi-workspace
   form (Cost, Metrics), or should all of Analytics stay single-workspace?
4. Does the Hosts page replace Settings › Hosts outright, or link to it for host-file edits?
5. What does the catalog show when two hosts each report the owner role for the same
   workspace name? The Mac and dk-server-2 both list `orbit-graph`, `nebula` and
   `observatory` that way today.

## Acceptance

- With 3 registered hosts and 12 logical workspaces, the picker lists 12 entries.
- A pull drain on any capacity host can be started and stopped under All workspaces
  using the existing Start a window card and Auto-drain Stop, without opening a replica board.
- `?workspace=a,b` shows rows from both workspaces in Tasks, Jobs, Leases and Automation, and
  a row action reaches the row's own workspace on its owner.
- An unreachable owner is named on the view and its rows are absent; no replica store is read.
- A `?host=H&workspace=W` link from before the change opens W.

## Task References

- [ORB-14680] added the host picker this spec retires.
- [ORB-10874] and [ORB-12235] added the workspace-qualified mutation rule for aggregate rows.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
