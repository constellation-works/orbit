---
title: Plugin Pages — Design
owner: claude
created_date: 2026-11-27
updated_date: 2026-11-27
last_validated: 2026-11-27
status: Draft
feature: plugin-pages
doc_role: design
type: design
summary: "Proposed design: the manifest key, the sandboxed frame and its policy, the postMessage bridge to a plugin's own tools, the Plugins sub-tabs and pages on other hosts"
tags: [plugin-pages, plugins, dashboard, orbit-web]
related_features: [plugins, user-interface, remote-access]
related_artifacts: [ORB-29504]
---

# Plugin Pages — Design

This doc is the proposed design, draft for the operator's approval ([ORB-29504]). Nothing in it
is built, so it describes what the first build makes true. The problem, the goal and the
questions open for approval are in the [overview](./1_overview.md). The plugin manifest,
grants and tool dispatch it builds on are the [plugin standard](../plugins/1_scope.md)'s.

## 1. The manifest

```yaml
spec:
  web:
    pages:
      - id: overview              # route: #plugins/<ns>/overview
        title: Formation          # the sub-tab's label
        root: web/overview        # a directory in the plugin root; index.html is the entry
        tools: [status, mail_log, roster_show]   # this plugin's tools the page may call
```

- **A page is a directory of built files.** HTML, script, styles, images and fonts, from a
  fixed list of extensions. How the plugin produced them is its own business: a bundler, a
  framework or three hand-written files. Orbit never runs a build for a page.
- **`tools` names only the plugin's own tools, and only `read_only` ones.** `orbit plugin
  validate` refuses any other name, a mutating tool, a missing `index.html`, a file type off
  the list, and a bundle over the size bound.
- **The bundle is covered by consent.** Install records a digest of each page's files beside
  the manifest digest. A load whose files do not match registers the page inactive with a
  diagnostic naming both digests, as a changed manifest does today (STD-05 §R21).

## 2. Serving

Orbit loads and hashes a page's files when the plugin loads and serves them from memory, so
the bytes served are the bytes that were hashed.

| Route | Does |
|---|---|
| `GET /api/plugins/<ns>/pages/<page>/assets/<path>` | One file of the bundle, with the content type its extension fixes |
| `POST /api/plugins/<ns>/pages/<page>/call` | Runs one tool the page declares, with input |
| `GET /static/plugin-kit/v1/…` | Orbit's own bridge script and stylesheet (below) |

Every asset response carries this policy, and the dashboard frames the page with the matching
`sandbox="allow-scripts"`:

```
sandbox allow-scripts; default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline';
img-src 'self' data: blob:; font-src 'self'; connect-src 'none'; form-action 'none';
base-uri 'none'; frame-ancestors 'self'
```

What that gives:

- **A unique origin.** The page cannot read the dashboard's document or storage, and a request
  it made to `/api/*` would carry `Origin: null`, which the router's origin guard already
  refuses.
- **No network.** `connect-src 'none'` stops fetches, sockets and event streams. The page loads
  its own files and Orbit's kit and nothing else.
- **No way out of the frame.** Without the other sandbox tokens it cannot open a window,
  navigate the dashboard, submit a form or start a download.

## 3. The bridge

The page talks to the dashboard by `postMessage`, and the dashboard talks to the server.

```
page    →  dashboard   {v: 1, id, call: "mail_log", input: {limit: 50, with: "0-chief"}}
dashboard →  server    POST /api/plugins/<ns>/pages/<page>/call
dashboard →  page      {v: 1, id, ok: true, output: …}   or   {v: 1, id, ok: false, error: {code, message}}
```

- **The server decides.** It checks that the page declares the tool, that the tool belongs to
  the plugin and is `read_only`, and that the input fits the tool's schema. Then it runs the
  call through the audited tool dispatch, which is what a panel read does today, plus input.
  The dashboard script is a relay and is not trusted to filter.
- **Calls are bounded.** Identical calls in flight share one execution. A page has a small
  number of executions at once, and one over the bound is refused with `busy`. Output is
  capped as a panel's is.
- **The dashboard tells the page where it is.** On load it sends the theme, the host, the
  workspace and whether the session is an operator's. A workspace or host change reloads the
  frame.
- **A view can be linked.** The page may ask the dashboard to set a query string on its hash
  (`#plugins/<ns>/<page>?…`) and receives it on load, since a sandboxed page has no storage of
  its own.

**The kit** is two files Orbit serves: a script that wraps the messages as `orbit.call(tool,
input)`, and a stylesheet with the dashboard's colours, type scale, spacing and table styles in
both themes. Using them is optional. They make a plugin's page look like the rest of the
dashboard without copying its CSS, and their path carries the bridge version.

## 4. Surfaces

- **Plugins gets secondary tabs,** as Jobs has. **Installed** (`#plugins`, the default) is
  today's list. After it comes one tab per page of each plugin enabled in the selected
  workspace, labelled with the page's title. The rail lists them as Plugins' views.
- **The dashboard draws a strip above the frame** with the plugin's name and version and a
  reload control. Everything below the strip is the plugin's.
- **A page that cannot load says why.** A plugin that is disabled, inactive or refused at load
  shows its diagnostic and the step that fixes it, as the Installed list does.
- **Listings.** `GET /api/plugins`, `orbit plugin show` and the Installed list name each
  plugin's pages. The terminal interface lists them and draws none.

## 5. Other hosts

With another host selected, its pages are that host's plugins. Assets and calls travel through
`/api/on/<host>/…` ([ORB-14679]) like every other read. The forwarder stamps its own policy on what it
relays, whatever the remote sent. For the page asset route it stamps the page policy above and
fixes the content type from the extension, so a remote page gets the same frame and no more.
The kit is always this dashboard's own.

## 6. Why no server per plugin

The first sketch had Orbit start a Node server for each plugin's page and show it in the tab.
This design does not, for these reasons:

| A server per plugin needs | Cost |
|---|---|
| Node on every host that serves a dashboard | Orbit installs as one binary with its dashboard inside. A second runtime becomes a requirement to check, version and repair on each machine |
| A long-lived process per plugin | A port, a supervisor, restarts and reaping (STD-03), and a listener that must validate `Host` and `Origin` on its own (STD-05 §R16–R17) |
| Plugin code that runs outside the plugin sandbox | The standard's grants bound the backend executable. A page server would be a second backend with none of them, or a second sandbox profile to design |
| A way to reach it from another machine | The tunnel carries the dashboard's port. Orbit would proxy each plugin's port, and what it proxied would still need the frame above to stay off the dashboard's origin |

A plugin already has a server side: its backend executable, behind tools. Whatever a page
server would compute, a tool can return. A plugin that wants its own server anyway keeps the
link tile it has today.

## 7. Configuration

No new key. A page shows when its plugin is enabled for the workspace, and enabling a plugin
whose manifest has pages lists them in the consent prompt.

## 8. Phasing

1. **Local read-only pages.** The manifest key, validation, the bundle digest, the asset and
   call routes, the frame, the bridge, the kit and the secondary tabs. Verified first against
   the three browser engines. The plugin standard's dashboard section and its non-goal are
   amended in the same change.
2. **Pages on other hosts.** The forwarder's page policy, and the decision on a read-only call
   from a dashboard without the operator session.
3. **Actions.** A page may name mutating tools under a separate `actions` list. Each call
   needs the operator session and a confirmation the dashboard draws outside the frame, naming
   the plugin, the tool and the input, and is audited with the web session. Enabling such a
   plugin asks consent for the list.
4. **First pages.** A Formation page and a Scribe page, each from its own repository, once
   their plugins exist.

## 9. Acceptance

- Given a plugin whose manifest declares a page, the operator opens `#plugins/<ns>/<page>` and
  sees the bundle's `index.html` drawn under the dashboard's strip, with no dashboard edit and
  no process started.
- Given a page that calls a declared read-only tool with input, the result reaches the page
  and the call appears in the audit trail as a call to that tool.
- Given a page that calls a tool its manifest does not list, a mutating tool, or another
  plugin's tool, the call is refused and nothing runs.
- Given a page script that fetches `/api/tasks`, reads the parent document or opens a window,
  each attempt fails in the browser.
- Given a bundle file changed on disk after install, the page is inactive with a diagnostic
  naming both digests, and the rest of the plugin still works.
- Given a manifest whose page names a mutating tool, has no `index.html` or exceeds the size
  bound, `orbit plugin validate` refuses it by name.
- Given another host selected, that host's page loads through the tunnel under the same frame
  policy.
- Given a plugin with panels and no pages, the Plugins tab behaves as it does today.

## 10. Concerns & Honest Limitations

- **The browser rules are not yet verified.** Whether `script-src 'self'` admits the bundle's
  own files in a frame with a unique origin, and whether module scripts and fonts load there
  without a cross-origin grant, differs by engine and has not been tested for this design.
  Phase 1 tests Chromium, Firefox and Safari before the manifest key is accepted. The fallback
  is a narrower contract (classic scripts, no web fonts beyond the kit's), not a wider origin
  guard.
- **A page can still mislead.** It draws anything inside its frame, including a form that asks
  for a secret it then shows on screen. It cannot send what is typed anywhere. The strip above
  the frame marks whose content it is.
- **Each call starts the backend.** An `exec` backend is a process per call, so a page that
  polls every second costs a process every second. The in-flight sharing and the bound cap the
  damage and do not make polling cheap.
- **No push.** A page learns of a change by asking again.
- **Another host's page needs an operator dashboard at first.** The forwarder refuses a `POST`
  without the operator session, and the call route is a `POST`. Phase 2 decides whether a
  read-only call is exempt.
- **Version skew.** A page written for bridge version 1 is served by whichever dashboard frames
  it. The kit's path and the message's `v` are the contract, and a host that does not know a
  version refuses the page with a diagnostic.

## Task References

- [ORB-14679] — added `/api/on/<host>/…` forwarding to another host's dashboard API.
- [ORB-29504] — this design.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
