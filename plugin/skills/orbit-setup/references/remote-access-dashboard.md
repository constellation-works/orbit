# Remote access: the dashboard

## Contents

- [Dashboard](#dashboard)
  - [Switching hosts in the dashboard](#switching-hosts-in-the-dashboard)

## Dashboard

```bash
orbit web serve --no-open
orbit web serve --port 8080 --no-open
orbit web connect <ssh-host>
orbit web connect <ssh-host> --remote-port 7878 --port 9000
orbit web connect <ssh-host> --workspace <remote-workspace-selector>
```

`orbit --root <ROOT> web serve` serves `<ROOT>/workspaces.json` and nothing from
the machine-global registry, so an explicit root isolates the dashboard the same
way it isolates every other command. `--workspace` sets a server hint. An
unmatched selector clears it. The server looks for a workspace containing its
current directory only when `--workspace` is omitted. `connect` forwards the
selector when it starts a remote server and rejects `--root`.

For a host with multiple owned workspaces, the browser tries a valid
`?workspace=` link first, including `?workspace=all`, then the choice remembered
for that host, including **All workspaces**. Next it tries a live drain on an
active owned workspace, then the server hint if it names an active owned
workspace. The fallback is the first active owned workspace in registry order.

If all owned workspaces are unavailable, the picker selects the first owned
entry in registry order and its board is unavailable. With exactly one owned
workspace, the browser opens it only if active. With zero owned workspaces,
there is no board to open.

The default dashboard port is 7878. `connect` reuses an existing remote loopback
server when available; otherwise it starts one and owns that process's lifetime.
The browser offers workspace selection, task detail and lifecycle controls,
run/step inspection, routines, knowledge/frictions, audit, and metrics. Verify the
selected workspace before any mutation; a dashboard aggregate or metric is not
proof that a particular task or run succeeded.

Global search sits in the top bar of the dashboard served by `orbit web serve`,
on every destination; press `/` or choose the search field. It finds tasks
across all statuses. For substring matches its scope follows the Workspace
picker: with **All workspaces** it searches across workspaces, and with one
workspace selected it searches only that workspace. Typing part of a task ID or
title lists compact matches.

Entering an exact task ID or run ID with Enter
opens the full task page or run page; that lookup also resolves across the
other active workspaces and selects the owning workspace when one holds the ID.
There is nothing to configure. To verify a match, type a known title
fragment, choose the suggested entry and confirm the full task page opens; then
enter a known run ID, press Enter and confirm the full run page opens.

In the left rail, the **Workspace** picker is single-select. It lists the workspaces
the selected host owns (a replica checkout is not listed), marks unavailable ones
*(unavailable)* and leaves them unchosen, and offers **All workspaces** as its
first option. Its button shows the chosen workspace's name. It shows **All
workspaces** only after that entry is chosen, through the picker, a shared link or
a remembered choice.

Choose a workspace to open its board, or choose **All
workspaces** for the aggregate view.

In the Tasks list, the **Workspaces** chip sits beside **More** and appears only
under **All workspaces** when the host has more than one active workspace. It is
multi-select: check or uncheck workspaces, and the Tasks board then contains only
the checked workspaces' tasks. Each task's workspace badge shows which workspace
it belongs to.

The chip reads **Workspaces** when every active workspace is
checked, and **N workspaces** while the selection narrows the board; its tooltip
lists the chosen names. Narrowing the checks to one workspace opens that
workspace's board.

Each Tasks row can show a task riskiness percentage: an estimate of whether a
task of that size, once done, will later be named by a regression. The estimate
uses task size alone, fitted on done tasks from the trailing 30 days. Before a
diff exists, size is the declared path count; once a review candidate or landed
diff resolves, size is the lines changed if a lines-based fit is available;
otherwise the estimate falls back to the declared path count.

The 30-day window
and the minimum of five done tasks are fixed. A `~` before the percentage means some selectors
name no path. If no estimate is available, the row has no risk part. Task detail
shows the estimate's basis and size, plus how many recent done tasks of that
size or larger regressed. When there is no estimate, detail gives one of four
reasons: **no files**, **no paths**, **thin history**, or **unavailable**.
Analytics uses this score.

`@auto` effort instead uses configured bands based
on complexity and declared files (or review diff lines), with the crew's
configured effort as a fallback; it does not read the riskiness score. To verify
the display, open one task with an estimate and one without; check each Tasks row and task detail
for the percentage or the matching no-estimate state and reason. There is
nothing to configure.

Under **All workspaces**, each status section lists tasks workspace by workspace
in Workspace-picker order, each workspace pill uses that workspace's colour, and
paging continues in that order. There is nothing to configure: choose **All
workspaces** to open the view. The **Workspaces** chip can narrow which
workspaces appear; the remaining rows stay in picker order.

To verify the order,
open Tasks under **All workspaces** and confirm each status section keeps one
workspace's rows together in picker order, and the next page continues in that
same order. To verify the colours, open the **Workspaces** chip: each workspace
pill matches its entry's colour in that list, which is the colour key. The rail
Workspace picker has no workspace colours, so use it for order only.

**Observe › Fleet** lists the drain rows across the registered hosts and their
workspaces in its **Drains** section. The workspace picker does not filter
Fleet, so the list is the same whichever workspace is selected.

The Tasks dock's **Drain** pane holds the **Auto-drain** card and, beside it, the
quick-start card headed **Start a window**. When the target is a replica checkout,
the heading and the toggle read **Start pulling on** followed by the host name. On
narrow layouts (1315px wide or less), a live window folds the quick-start card
behind **Start another window**, which expands the form.

Under **All workspaces**,
pick the target in the Auto-drain panel's **Drain target** select, whose options
are workspace-host pairs such as `site · owner-host (owner)` and
`site · follower-host (pull)`, before starting a window.

A visible dashboard with a workspace selected opens a change stream for that
workspace; **All workspaces** has no single-workspace stream. In the Tasks tab,
the connected stream sends change notices to the panels that subscribe to them,
and a full refresh uses a three-minute safety interval while refreshes succeed.
Other tabs keep the regular 30-second refresh even while a workspace stream is
open. **All workspaces** has no stream and uses that same 30-second interval.

If the stream is unavailable, Tasks also keeps polling at the regular cadence;
a fully closed stream is retried after 60 seconds. A failed full refresh doubles
the normal 30-second interval up to five minutes, and a successful refresh
resets that normal interval to 30 seconds.
With the Tasks stream connected, the safety poll still keeps a three-minute
floor between periodic full refreshes.

In a background browser tab, the dashboard closes the stream and refreshes no
panels. It checks failed runs and, for a selected workspace, drain state and
red-base holds once a minute. The page title gains a `(N failed)` prefix for
runs that failed while the tab was hidden, cleared when the tab is shown again.

**Notify** in the top bar opts in to a browser notification when a drain ends or
a task is newly held on a red base while the tab is hidden; the browser asks for
permission only on that click. Showing the tab refreshes the dashboard and
reopens the stream when a concrete workspace is selected.

### Switching hosts in the dashboard

A host registered with `orbit host add` appears in the dashboard's **Host**
picker, above the workspace picker, so one dashboard can show each registered
machine without an `orbit web connect` tab per machine. Prefer `connect` when
the machine is not registered, when you need `--no-operator` or a non-default
`--remote-port`, or when the dashboard must not depend on the serving machine.

- The serving dashboard reaches the host over its own SSH identity, not the
  browser's. It attaches to a dashboard already running on the remote's default
  port 7878 or starts one, runs SSH with `BatchMode=yes`, and closes an idle
  tunnel after five minutes. A host that needs a passphrase or password
  therefore reports `unreachable_destination`.
- `?host=<name|machine_id>` selects the host in the URL and wins over the
  browser's remembered last choice, which only fills a URL with no `?host=`.
  The serving host's own name selects the serving host. Back and Forward
  restore the host each history entry names, and an entry without `?host=`
  is the serving host.
- Every panel, action, log tail and resource chip follows the selected host,
  and the workspace picker lists that host's workspaces. **Observe › Fleet**
  keeps editing the serving host's `hosts.toml`. Each reachable registered host
  has a card with its load, memory, disk, I/O and admission state, and the
  **Drains** section lists its drains and pull drains. Select the host's card,
  then **Show** in its dialog, to switch the dashboard to that host.

  An
  unreachable host shows its error instead of readings.
- Version or protocol skew shows a persistent banner and is never refused.
  An unreachable host replaces the panels with one state carrying a code:
  `unknown_host`, `unreachable_destination`, `process_timeout`,
  `host_identity_mismatch` or `host_too_old`, with Retry and a way back to the
  serving host.
- Where a task's execution line says which machine ran it and that machine is
  registered on the serving host, the name links to the run on that host.

Remote writes need an operator session on the serving dashboard
(`orbit web serve --operator`, or `connect` without `--no-operator`). Without
one the dashboard shows `Read-only on <host>` and disables write controls; a
direct request gets `403 authorization_denied` for `host.forward` before any SSH
starts. A remote dashboard Orbit starts gets `--operator` exactly when the
serving session has it, and one already running keeps its own capability.

The dashboard refuses non-loopback binds and has no application login. Origin
checks and `Sec-Fetch-Site` checks mitigate browser CSRF, not unauthorized port
access. When present, `Sec-Fetch-Site` must be `same-origin` or `none`; direct
CLI/curl requests without the header continue to work. Older browsers that
omit both Fetch Metadata and `Origin` on GETs do not receive the additional
Fetch Metadata protection.

Anyone with access to the forwarded port can reach
mutation endpoints with the server's application authority. Keep access within
the intended operator boundary.
