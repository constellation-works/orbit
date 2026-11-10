---
type: design
summary: "Spec: Workspace Navigation Across Hosts"
tags: ["user-interface", "host-registry", "distributed-drain"]
last_validated: 2026-11-09
---

# Spec: Workspace Navigation Across Hosts

> **Proposed, not implemented.** This spec describes the target. Until it ships,
> [2_design.md](../2_design.md) describes the dashboard as it is.

The dashboard navigates by logical workspace, never by host and workspace together. Each
workspace appears once, its requests go to the host that owns it, and the hosts that hold a
replica checkout of it appear as drain capacity on the owner's board. One scope, a set of
workspaces plus the time window, applies to every view.

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
4. The workspace picker lists each catalog entry once, with the owner's name as a badge.
   A replica checkout never produces its own entry.
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
   place: to `?workspace=W` when H owns W, and to `?workspace=W` with the Drain dock open on
   H's lane when H is a capacity host of W. The remembered host is discarded.
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

## 4. Capacity lanes

With one workspace in scope, the Drain card in the Tasks dock shows one lane per host that
can run the workspace: the owner's lane first, then each capacity host in host-file order.

1. A lane reads `GET /api/workflows/auto/readiness` for the workspace on its own host,
   through the forward for a remote host.
2. A lane shows the host name, its role (owner or follower), the phase (Idle, Draining,
   Winding down, Pull drain or Read-only), used and ceiling leaf slots, holds, and the
   host's usage pressure.
3. **Start** on the owner's lane starts a drain there. **Start** on a follower lane posts the
   same start request to that host, where a replica checkout pulls from the owner. The server
   contract is unchanged and is the one `orbit run auto --pull` validates.
4. **Stop** on a lane posts `POST /api/workflows/auto/stop` to that lane's host and keeps
   today's confirmation text. **Send pending results** stays on the lane it applies to.
5. A lane whose host refuses a start (`start_options.pull_refusal`, missing owner routing)
   shows the refusal and disables Start. A lane whose host does not answer shows
   **unreachable** and offers no controls.
6. A lane's run link opens the run on the host that executes it.
7. With more than one workspace in scope the card shows the fleet drain table grouped by
   workspace, with one sub-row per host. Selecting a row narrows the scope to that workspace;
   it does not switch host. The per-host tabs are removed.

## 5. Host as a facet

1. Runs, leases, worktrees and log tails name the host they belong to. Jobs and Leases have a
   host filter beside the workspace filter.
2. The run-detail route carries the executing host, so a link to a follower's run resolves
   through the forward without changing the scope.
3. The rail's drain reading counts live lanes across the scope (`2 live`), on every view.

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

| Phase | Change | Removes |
|---|---|---|
| 1 | Catalog and owner routing (§1, §2 rules 1 to 3 and 5). The host picker gains **All hosts** as its first and default entry, which shows the catalog. Choosing one host still shows that host's own board, as today, so a follower's pull drain stays reachable. | Duplicate workspace entries in the default view. |
| 2 | Capacity lanes (§4), the host facet (§5) and `?host=` normalization (§2 rule 4). The picker's single-host entries no longer open a board. | The need to open a follower board. |
| 3 | Multi-workspace scope in every view (§3). | The aggregate placeholder. |
| 4 | Hosts page; the host picker is removed (§6). | Host as a navigation level. |

Phases 1 and 2 end the 36-board problem. Phase 3 is the largest: it changes each aggregate
endpoint and each list view.

## Mocks

Static HTML mocks for phases 1 and 2 are in [`../references/mocks/`](../references/mocks/).
Open a file in a browser at the stated width. They show layout, wording and states; the
workspace names, slot counts, run IDs and times are illustrative. Where a mock and a rule in
this spec disagree, the rule wins.

| Mock | Width | Shows | Rules |
|---|---|---|---|
| [1.1](../references/mocks/1-1-tasks-all-hosts-catalog.html) | 1280 | Tasks under All hosts with the workspace catalog open | §1, §2 |
| [1.2](../references/mocks/1-2-picker-states.html) | 1000 | Host picker default, catalog loading, an owner that does not answer, one host chosen | §1 rules 5 and 6, Phasing |
| [1.3](../references/mocks/1-3-owner-unreachable.html) | 880 | The Tasks view when the owner is unreachable | §1 rule 5 |
| [2.1](../references/mocks/2-1-drain-capacity-lanes.html) | 1280 | The Drain card with one lane per host and the pull-window form | §4 rules 1 to 4 |
| [2.2](../references/mocks/2-2-lane-states.html) | 560 | Lane states: winding down, start refused, unreachable, pending results, read-only | §4 rules 4 and 5 |
| [2.3](../references/mocks/2-3-all-workspaces-drains.html) | 1280 | All workspaces: drains grouped by workspace, one sub-row per host | §4 rule 7 |
| [2.4](../references/mocks/2-4-jobs-host-filter.html) | 1280 | Jobs with a host column and host filter | §5 |
| [2.5](../references/mocks/2-5-capacity-lanes-phone.html) | 390 | The capacity lanes at phone width | §4 |

**Keep the current Tasks board CSS for the most part.** The mocks approximate the board,
the rail and the dock; they are not a restyle. Build the new pieces from the classes the
dashboard already has: the board's `.row`, `.group-header` and `.chip`, the dock's `.dock-*`,
and the Drain card's `.drain-*` (its slot meter is `.drain-capacity`). Add CSS only for what is
new: catalog entries in the workspace picker, a lane row, and a host sub-row under a workspace.

Two things the mocks show that the rules above do not state:

- When an owner does not answer, the catalog knows its workspaces only through other hosts'
  replica rows. Workspaces that only the owner holds are not listed, and the picker says so
  (mock 1.2).
- The pull-window form on a follower lane offers window length and parallel tasks only
  (mock 2.1). Check this against the start contract before building it; the contract decides.

## Concerns and Honest Limitations

- **A scope spanning owners costs one request per owner per view.** With the three-request
  limit and a slow host, a list view fills in visibly later for that owner's rows.
- **Merged paging is approximate.** Until every owner has returned its page, the merged order
  can change as rows arrive.
- **The serving host must reach the owner.** A serving host that can reach only a follower
  shows that workspace as owner unreachable, where today it could show the follower's board
  and its pull drain controls. The capacity lane for a reachable follower still works.
- **Saved scopes are per browser.** They do not follow the operator to another machine.

## Open Questions

1. Which follower-run facts does the owner already record? A lane's run link and the host
   facet need the executing host on run and lease rows. If the owner holds only the claim,
   Jobs has to merge the capacity hosts' run lists into the owner's.
2. Should saved scopes live in the serving host's configuration so they are shared across
   browsers?
3. Do the Analytics views that are single-workspace today have a meaningful multi-workspace
   form (Cost, Metrics), or should all of Analytics stay single-workspace?
4. Does the Hosts page replace Settings › Hosts outright, or link to it for host-file edits?

## Acceptance

- With 3 registered hosts and 12 logical workspaces, the picker lists 12 entries.
- A pull drain on any capacity host can be started and stopped from the owner's Tasks dock
  without changing host.
- `?workspace=a,b` shows rows from both workspaces in Tasks, Jobs, Leases and Automation, and
  a row action reaches the row's own workspace on its owner.
- An unreachable owner is named on the view and its rows are absent; no replica store is read.
- A `?host=H&workspace=W` link from before the change opens W.

## Task References

- [ORB-14680] added the host picker this spec retires.
- [ORB-10874] and [ORB-12235] added the workspace-qualified mutation rule for aggregate rows.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
