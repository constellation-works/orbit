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

With one workspace in scope, the Auto-drain card in the Tasks dock gains a Hosts list with one
row (a lane) per host that can run the workspace: the owner first, then each capacity host in
host-file order. The rest of the card is unchanged.

1. A lane reads `GET /api/workflows/auto/readiness` for the workspace on its own host,
   through the forward for a remote host.
2. A lane shows the host name, its role (owner or follower), the phase (Idle, Draining,
   Winding down, Pull drain or Read-only) and used and ceiling leaf slots. It is the row the
   Fleet drains card already uses.
3. The owner's Start, Stop and run link stay where they are today: the card's foot and the
   Start a window card. A follower lane carries **Start** or **Stop**. Start on a follower
   lane opens the Start a window card for that host, with the same fields, and posts the same
   start request to that host, where a replica checkout pulls from the owner. The server
   contract is unchanged and is the one `orbit run auto --pull` validates.
4. **Stop** on a lane posts `POST /api/workflows/auto/stop` to that lane's host and keeps
   today's confirmation text. **Send pending results** stays on the lane it applies to.
5. A lane whose host refuses a start (`start_options.pull_refusal`, missing owner routing)
   shows the refusal and disables Start. A lane whose host does not answer shows
   **unreachable** and offers no controls.
6. A lane's run link opens the run on the host that executes it.
7. With more than one workspace in scope the Fleet drains card shows one row per workspace,
   and the row's existing detail line names each host's phase and slots. Selecting a row
   narrows the scope to that workspace; it does not switch host. The per-host tabs are
   removed.

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

The first delivered step is deliberately smaller than the catalog phase below:
on one host, the rail workspace dropdown becomes a persisted, URL-addressed
checkbox multi-select (one tick keeps the existing board; multiple ticks narrow
All workspaces task rows and counts). All workspaces gains an owner-first
Auto-drain host select and a separate Fleet dock tab. It adds no All hosts entry,
catalog or owner routing, and does not collapse follower workspaces. This step
precedes and replaces the original phase 1 for now; the remaining phases below
are future work.

| Phase | Change | Removes |
|---|---|---|
| 1 | Catalog and owner routing (§1, §2 rules 1 to 3 and 5). The host picker gains **All hosts** as its first and default entry, which shows the catalog. Choosing one host still shows that host's own board, as today, so a follower's pull drain stays reachable. | Duplicate workspace entries in the default view. |
| 2 | Capacity lanes (§4), the host facet (§5) and `?host=` normalization (§2 rule 4). The picker's single-host entries no longer open a board. | The need to open a follower board. |
| 3 | Multi-workspace scope in every view (§3). | The aggregate placeholder. |
| 4 | Hosts page; the host picker is removed (§6). | Host as a navigation level. |

Phases 1 and 2 end the 36-board problem. Phase 3 is the largest: it changes each aggregate
endpoint and each list view.

## Mocks

**Change as little as possible.** The dashboard's current look is settled. Each change in
phases 1 and 2 is made inside a component that already exists, with its existing markup and
classes, and everything around it stays as it is. New CSS is limited to the four rules shown
in mock 2.1.

The mocks in [`../references/mocks/`](../references/mocks/) are subsections, not whole
screens. Each file shows one component as it is today beside the proposed change. The markup
is copied from the running dashboard and the files link the dashboard's own stylesheets from
`crates/orbit-web/assets/dashboard/css`, so open them from a checkout. The data is
illustrative.

| Mock | Component | Change | New CSS |
|---|---|---|---|
| [1.1](../references/mocks/1-1-pickers.html) | Host select and workspace listbox in the rail | Host gains All hosts; the workspace listbox lists each workspace once under owner group labels; unreachable and loading entries | None |
| [1.2](../references/mocks/1-2-owner-unreachable.html) | Tasks list empty state | The replica empty state, reworded for an unreachable owner | None |
| [2.1](../references/mocks/2-1-drain-card-hosts.html) | Auto-drain card and Start a window card | A Hosts list of fleet rows between the slot bar and the foot; follower rows carry Start or Stop; the Start card names the host | Four rules |
| [2.2](../references/mocks/2-2-fleet-drains.html) | Fleet drains card | Host tabs removed; one row per workspace; the detail line names each host | None |

Not mocked: the host column and filter on Jobs and Leases (§5). They follow the same rule
when they are built.

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
5. What does the catalog show when two hosts each report the owner role for the same
   workspace name? The Mac and dk-server-2 both list `orbit-graph`, `nebula` and
   `observatory` that way today.

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
