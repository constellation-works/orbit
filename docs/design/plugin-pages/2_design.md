---
title: Plugin Pages — Design
owner: claude
created_date: 2026-11-27
updated_date: 2026-11-28
last_validated: 2026-11-28
status: Accepted
feature: plugin-pages
doc_role: design
type: design
summary: "Approved design; phase 1 (manifest, serving, bridge relay, kit and Plugins sub-tabs), phase 2 (pages on other hosts, written here from their parts) and phase 3 (actions behind consent and a dashboard-drawn confirmation) are built"
tags: [plugin-pages, plugins, dashboard, orbit-web]
related_features: [plugins, user-interface, remote-access]
related_artifacts: [ORB-29504, ORB-29538, ORB-29539]
---

# Plugin Pages — Design

The operator approved this design on 2026-11-27 ([ORB-29504]). The page contract in sections 1
and 2 was revised after it was measured in two browser engines ([ORB-29532]), and the operator
approved the revision the same day ([ORB-29573]).
Section 1 (the manifest), section 2 (serving), section 3 (the bridge, kit and actions) and
section 4 (Plugins sub-tabs, frame strip and diagnostics) and section 5 (other hosts) are built
([ORB-29534], [ORB-29536], [ORB-29537], [ORB-29538], [ORB-29539]). The rest describes what later
phases make true.
The problem, the goal and the build tasks are in the [overview](./1_overview.md). The plugin
manifest, grants and tool dispatch it builds on are the
[plugin standard](../plugins/2_design.md)'s.

## 1. The manifest

```yaml
spec:
  web:
    pages:
      - id: overview              # route: #plugins/<ns>/overview
        title: Formation          # the sub-tab's label
        root: web/overview        # a directory in the plugin root; index.html is the markup
        scripts: [app.js]         # files under root, written into the page in this order
        styles: [app.css]         # files under root, written into the page in this order
        kit: 1                    # optional: Orbit's bridge script and stylesheet, by version
        tools: [status, mail_log, roster_show]   # this plugin's tools the page may call
        actions: [request_settle]                 # optional: its mutating tools, run as actions
```

- **A page is markup, scripts and styles, built ahead.** `index.html` holds the markup and no
  `script` or `link` element. The scripts and styles are the files the manifest lists. How the
  plugin produced them is its own business: a bundler, a framework or three hand-written
  files. Orbit never runs a build for a page.
- **A page is self-contained.** Its frame fetches nothing ([Serving](#2-serving)), so a script
  imports no file at run time, and an image or a font is a `data:` URL inside the markup or a
  style. A file whose name ends in `.mjs` is written as a module script; any other script is a
  classic one.
- **`tools` names only the plugin's own tools, and only `read_only` ones.** `orbit plugin
  validate` refuses any other name and a mutating tool.
- **`actions` names only the plugin's own mutating tools.** `orbit plugin validate` refuses
  any other name, a `read_only` tool (which belongs under `tools`) and a name listed twice.
  An action runs only as [The bridge](#3-the-bridge) describes.
- **The operator consents to the actions.** `orbit plugin add --enable` and `orbit plugin
  enable` list each page's actions. The host-owned grant witness records a digest for each
  approved page and verb covering the manifest, the page's file bundle, and the tool's
  resolved schemas. An action needs a matching digest as well as a declaration on its page
  (STD-05 §R21). A plain forced reinstall that changes this content leaves its actions
  inactive until CLI consent. The dashboard's host re-enable preserves the previous
  consent, and older witnesses that record only page IDs and verbs require fresh CLI
  consent. The plugin and its read-only page calls stay available while otherwise valid.
- **`orbit plugin validate` refuses a page Orbit could not write out safely,** by name: a
  missing `index.html` or listed file; markup that holds a `script` or `link` element, which
  the browser would refuse anyway with nothing shown to the author; a script that contains
  `</script` or `<!--`, or a style that contains `</style`, either of which would end its
  element early; a file listed twice; a symbolic link; a root or listed file outside the
  plugin root; a `kit` version Orbit does not know; a duplicate page id; and a page over the
  size bound. The bound is 2 MiB (2,097,152 bytes) counted over `index.html` and the listed
  files as read, and the refusal states it.
- **Only listed files count.** A file in `root` the manifest does not list is never loaded,
  hashed or served, so it changes neither the digest nor the page's state.
- **The bundle is covered by consent.** Install records a digest of each page's files beside
  the manifest digest: SHA-256 over `index.html`, then each script and style in manifest
  order, each framed by its kind, name and length. A load whose files do not match registers
  that page inactive with a diagnostic naming both digests, as a changed manifest does today
  (STD-05 §R21); so does a page whose files now fail a check above. The rest of the plugin
  keeps loading, and the recovery is the forced reinstall a changed manifest needs.
- **The operator sees pages before consenting.** The preview is `orbit plugin add <source>`
  without `--enable`, then `orbit plugin show <name>` (or `GET /api/plugins`). `add` installs
  the plugin disabled and names each page, the tools it may call and its actions; it records
  no grant witness, so every action reads `consented: false` and `active: false`. `show` and
  the API list `pages[]` with id, title, tools, `actions[]` (each with `tool`, `consented`
  and `active`), kit version and the active or inactive state with its diagnostic, and
  change nothing. Only then does the operator run `orbit plugin enable <name>`.
- **`enable` and `add --enable` report consent; they do not preview it.** Each runs the
  mutating command first, so its page and action listing reports the list the command has
  just recorded in the grant witness, and the actions are already consented. There is no dry
  run. An operator who wants to inspect the actions before consenting stops at the preview
  above.

## 2. Serving

Orbit loads and hashes a page's files when the plugin loads and keeps them in memory, so the
bytes served are the bytes that were hashed.

| Route | Does |
|---|---|
| `GET /api/plugins/<ns>/pages/<page>/` | The page, as one document |
| `GET /api/plugins/<ns>/pages/<page>/parts` | The same page's parts as JSON, for another host's dashboard to write ([Other hosts](#5-other-hosts)) |
| `POST /api/plugins/<ns>/pages/<page>/call` | Runs one tool the page declares, with input |
| `POST /api/plugins/<ns>/pages/<page>/actions/confirm` | Checks one action and issues its one-time confirmation; runs nothing |
| `POST /api/plugins/<ns>/pages/<page>/actions/run` | Spends the confirmation and runs the action once |
| `POST /api/plugins/<ns>/pages/<page>/actions/cancel` | Releases a confirmation the operator did not use; runs nothing |

**Orbit writes the document.** In order: the kit's stylesheet and each style, each in a
`style` element; the markup; the kit's script and each script, each in a `script` element.
Nothing in a page file is parsed or rewritten. There is no route for a single file, because
the frame never asks for one.

The dashboard shell serves an explicit `frame-src 'self'; child-src 'self'` policy, the
dashboard frames the page with `sandbox="allow-scripts"` and `referrerpolicy="no-referrer"`,
and the page response carries this policy:

```
sandbox allow-scripts; default-src 'none'; script-src 'sha256-<kit>' 'sha256-<script>' …;
style-src 'unsafe-inline'; img-src data: blob:; font-src data:; connect-src 'none';
frame-src 'none'; object-src 'none'; form-action 'none'; base-uri 'none'; frame-ancestors 'self'
```

`GET /api/plugins/<ns>/pages/<page>/` can be refused at two points, in order. The API origin guard
(`require_localhost_origin`) runs before the page handler. It refuses a `Sec-Fetch-Site`
other than `same-origin` or `none`, so `cross-site` (the value opaque-origin
self-navigations send) is refused there, and it refuses an `Origin` that does not match the
admitted host, including `null`. That refusal is a plain `403` with the body
`{"error": "cross-origin requests not allowed"}`. It carries no `code`, no `no-store` and no
sandbox `Content-Security-Policy`; the only header it adds is the `nosniff` that every `/api`
response carries. Requests the guard admits reach the page handler, which requires Fetch
Metadata proving a same-origin dashboard iframe navigation (`Sec-Fetch-Site: same-origin`,
`Sec-Fetch-Dest: iframe`, `Sec-Fetch-Mode: navigate`). Direct top-level browser loads
(`Sec-Fetch-Site: none`), same-origin non-navigation fetches, and unsupported browsers or
proxies that omit Fetch Metadata fail closed there with `403` and `code: "frame_required"`,
plus `no-store` and `Content-Security-Policy: sandbox; default-src 'none'`.

`script-src` lists one SHA-256 hash per script Orbit writes, computed from its
HTML-preprocessed UTF-8 text: the bytes are decoded lossily as UTF-8, with CRLF and lone
CR normalized to LF. The document still writes each script's original raw bytes unchanged.
The policy is therefore fixed by the bundle and the kit version. The document is written
once for that pair, the bundle digest and the kit version, from the bytes the plugin
load hashed and served from memory, as `text/html; charset=utf-8` with `nosniff` and
`no-store`. A change to either input replaces the cached document. That includes a kit
change on a running server whose page files, and therefore the bundle digest, stay the
same. An unknown page, a page whose files fail their checks or digest, and a plugin
that is inactive or disabled for the workspace each answer 404 with the reason.

What that gives:

- **A unique origin.** The page cannot read the dashboard's document or storage, and the
  bridge relay requires both `event.source === activeFrame.contentWindow` and
  `event.origin === "null"`.
- **One host-owned document.** The dashboard starts the initial load on a fresh `<iframe>`
  (and dashboard Reload replaces the `<iframe>` with a new one), so each legitimate load is a
  same-origin iframe navigation admitted by both the origin guard and the Fetch Metadata gate.
  A page cannot load outside the dashboard frame, and any subsequent `load` event on the same
  `<iframe>` or parent `frame-src`/`child-src` `securitypolicyviolation` tears down the frame,
  revokes the bridge, and shows a diagnostic card with Reload recovery. Same-document fragment
  navigation (`#hash`) stays inside the document without triggering teardown.
- **Only the listed scripts run.** A script that reached the page another way has no hash in
  the policy: one written into the markup, one a tool's output smuggled in, or one a running
  script adds. `eval` is refused as well.
- **No direct external network from the frame.** `connect-src 'none'` stops fetches, sockets
  and event streams from the frame itself, `frame-src 'none'; object-src 'none'` blocks nested
  frames and objects inside the page, and no directive admits a subresource from any server.
  Because WHATWG HTML `sandbox="allow-scripts"` still permits a frame's own navigation and CSP
  `navigate-to` is unimplemented across browsers, the external navigation boundary is enforced
  by the dashboard host's `frame-src 'self'` (blocking external origins, `data:`, and `blob:`
  via `location`, links, or `<meta http-equiv="refresh">` before a request leaves the browser)
  together with the server's iframe-only Fetch Metadata gate (preventing unframed top-level
  loads that would lack that host policy). Same-origin self-navigations from the opaque-origin
  frame cannot reach `/api/*` or `/healthz` (`Sec-Fetch-Site: cross-site` answers `403`) or
  render dashboard shell/static routes (`frame-ancestors 'none'`), and trigger immediate frame
  teardown on load. This restricts direct requests only: the declared tool-call bridge in
  [The bridge](#3-the-bridge) can still pass schema-valid page input to the plugin backend
  through the dashboard-relayed `POST .../call`.
- **No way out of the frame.** Without the other sandbox tokens it cannot open a window,
  navigate the dashboard, submit a form or start a download.
- **Supported browsers fail closed when missing prerequisites.** Enforcement requires a
  browser supporting CSP `frame-src` + `sandbox` and Fetch Metadata (`Sec-Fetch-Site`,
  `Sec-Fetch-Dest`, `Sec-Fetch-Mode` — Chromium 76+, Firefox 90+, Safari 16.4+). Older
  browsers or intermediaries that strip Fetch Metadata fail closed with `403` (`frame_required`).

## 3. The bridge

The page talks to the dashboard by `postMessage`, and the dashboard talks to the server.

```
page    →  dashboard   {v: 1, id, call: "mail_log", input: {limit: 50, with: "0-chief"}}
dashboard →  server    POST /api/plugins/<ns>/pages/<page>/call
dashboard →  page      {v: 1, id, ok: true, output: …}   or   {v: 1, id, ok: false, error: {code, message}}
dashboard →  page      {v: 1, init: {theme, host, workspace, operator, query}}
page    →  dashboard   {v: 1, query: "…"}
page    →  dashboard   {v: 1, id, act: "request_settle", input: {request: 268}}
```

The call route takes `{tool, input}` and answers with a panel read's body (`output`, plus
`truncated` and `diagnostic` over the cap). A refused tool is a 403 `tool_refused`, input
that does not fit the schema a 400 `input_invalid`, and a call over the bound a 429 `busy`;
none of them runs anything.

- **The server decides.** It checks that the page declares the tool, that the tool belongs to
  the plugin and is `read_only`, and that the input fits the tool's schema. Then it runs the
  call through the audited tool dispatch, which is what a panel read does today, plus input.
  The dashboard script is a relay and is not trusted to filter.
- **Calls are bounded.** Identical calls in flight share one execution. A page has at most
  four executions at once in a workspace, and one over the bound is refused with `busy`.
  Output is capped as a panel's is.
- **The dashboard tells the page where it is.** On load it sends the theme, the host, the
  workspace and whether the session is an operator's. A workspace or host change reloads the
  frame.
- **A view can be linked.** The page may ask the dashboard to set a query string on its hash
  (`#plugins/<ns>/<page>?…`) and receives it on load, since a sandboxed page has no storage of
  its own.

**An action** is a page asking to run one of its `actions`. The operator's intent is
established outside the page ([decision](./4_decisions.md#plugin-code-acts-only-behind-a-confirmation-drawn-outside-its-frame)):

1. The dashboard sends `{tool, input}` to `.../actions/confirm`. The server requires the
   operator session, checks that the tool is this plugin's and mutating, that the page
   names it under `actions`, that its content matches the recorded consent and that the
   input fits the schema. It answers with what the dialog shows (plugin and version, page,
   canonical tool name, description, input) and a random one-time confirmation bound to
   this workspace runtime, plugin, page, tool and input. It holds at most 32 unused ones,
   each for five minutes; over the bound it answers 429 `busy`. Nothing runs.
2. The dashboard draws a modal dialog in its own document, naming the plugin, the tool and
   the input from the server's answer. The dialog sits in the browser's top layer and makes
   the rest of the document, the frame included, inert, so the page cannot draw over it,
   dismiss it or confirm it. Run is armed only after the dialog has been visible for
   800 ms, and only a trusted click confirms; Cancel, Escape and the frame unloading cancel.
   One action waits for the operator at a time, from its check to the operator's answer,
   and a page asking again meanwhile gets `busy` before it can take a confirmation. A
   confirmation the operator did not use (Cancel, Escape, the frame unloading or changing)
   is released at once by `.../actions/cancel`, sent with the same `{tool, input,
   confirmation}`; it frees the slot only for the request it was issued for, answers
   `released: false` for any other, and runs nothing.
3. On Run, the dashboard sends `{tool, input, confirmation}` to `.../actions/run`. The
   server spends the confirmation first, whatever the request turns out to be, then
   requires the operator session again, refuses a missing, unknown, used, expired or
   differently bound confirmation with 403 `confirmation_refused`, repeats the step-1
   checks, and runs the tool once through the audited dispatch with operator capability
   and the web session as its origin.

Every run request, refused or run, writes a `dashboard.operations` row recorded as
`plugin.page_action`, with the web session, the plugin as target and the page and tool
as arguments; the tool call that runs writes its own row too. A refused action or a
cancelled dialog reaches the page as `{ok: false, error: {code}}` (`cancelled` for a
cancel), and a run as the tool's output, capped as a call's is. A page cannot reach a
mutating tool through `call`, which still refuses one with `tool_refused`.

**The kit** is two parts Orbit writes into the document when the manifest names `kit`: a
script that wraps the messages as `orbit.call(tool, input)` and `orbit.act(tool, input)`,
and a stylesheet with the
dashboard's colours, type scale, spacing and table styles in both themes. Using them is
optional. They make a plugin's page look like the rest of the dashboard without copying its
CSS. The manifest's `kit` number is the bridge version the page was written for.

## 4. Surfaces

- **Plugins gets secondary tabs,** as Jobs has. **Installed** (`#plugins`, the default) is
  today's list. After it comes one tab per page of each plugin enabled in the selected
  workspace, labelled with the page's title. The strip stays on one line. Pages that do not
  fit at a readable width move under **More**, and the open page stays in the strip. A title
  cut short shows its full text on hover, focus and touch. The rail lists every page as
  Plugins' views.
- **The dashboard draws a strip above the frame** with the plugin's name and version and a
  reload control. Everything below the strip is the plugin's.
- **A page that cannot load says why.** A plugin that is disabled, inactive or refused at load
  shows its diagnostic and the step that fixes it, as the Installed list does.
- **Listings.** `GET /api/plugins`, `orbit plugin show` and the Installed list name each
  plugin's pages. The terminal interface lists them and draws none.

## 5. Other hosts

With another host selected, its pages are that host's plugins, and calls travel through
`/api/on/<host>/…` ([ORB-14679]) like every other read. The document does not travel whole.
The forwarder stamps its own policy on what it relays, whatever the remote sent, and it cannot
vouch for script hashes it did not compute. So the remote host gives the page's parts (markup,
scripts, styles) as data, and this dashboard writes the document and its policy itself, with
its own kit. A remote page gets the same frame as a local one and no more.

- **The parts route.** Every host answers `GET /api/plugins/<ns>/pages/<page>/parts` with
  `{encoding: "base64", kit, markup, scripts: [{name, content}], styles: [{name, content}]}`:
  the bytes of `index.html` and of each listed script and style in manifest order, and the
  page's `kit` version. It answers for exactly the pages the document route serves, with the
  same 404s, and needs no Fetch Metadata because nothing renders it. Through the forwarder it
  is ordinary relayed data under the forwarder's policy.
- **The forwarder writes the document.** For `GET /api/on/<host>/plugins/<ns>/pages/<page>/`
  it applies the document route's Fetch Metadata gate, reads the remote's parts route
  instead, and writes the document with the code a local page uses: this dashboard's kit,
  policy and hashes. The remote's headers are ignored. A refusal from the remote, such as a
  404 for a disabled page, is relayed as any other response.
- **The parts are not trusted.** Before writing, the forwarder applies the refusals a local
  page's files get at load ([The manifest](#1-the-manifest)): `<script` or `<link` in the
  markup, `</script` or `<!--` in a script, `</style` in a style, a script name not ending in
  `.js` or `.mjs`, a style name not ending in `.css`, the size bound, and a `kit` version this
  dashboard does not know. A page that fails gets a `502` `page_refused` diagnostic naming the
  host and the reason, under `sandbox; default-src 'none'`, in its place and no document.
- **Every other forwarded response keeps the forwarder's policy.** The dashboard builds a
  remote page's frame and call URLs under `/api/on/<host>/…`.

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
whose manifest has pages lists them in the report of the consent it records; the
[preview](#1-the-manifest) before consenting is `plugin add` without `--enable`, then `plugin show`.

## 8. Phasing

1. **Local read-only pages.** The manifest key, validation, the bundle digest, the document
   and call routes, the frame, the bridge, the kit and the secondary tabs. The three-engine
   probe for this phase was completed in Chromium, WebKit and Firefox
   ([browser evidence](#10-concerns--honest-limitations)). The plugin standard's dashboard
   section and its non-goal are amended in the same change.
2. **Pages on other hosts** (built, [ORB-29538]). The route that gives a page's parts, the
   forwarder writing the document from them, and the call-route rule for a dashboard without
   the operator session ([§10](#10-concerns--honest-limitations)).
3. **Actions** (built, [ORB-29539]). A page may name mutating tools under a separate
   `actions` list. Each call needs the operator session and a confirmation the dashboard
   draws outside the frame, naming the plugin, the tool and the input, and is audited with
   the web session. Enabling such a plugin records consent for the list.
4. **First pages.** A Formation page and a Scribe page, each from its own repository, once
   their plugins exist.

## 9. Acceptance

- Given a plugin whose manifest declares a page, the operator opens `#plugins/<ns>/<page>` and
  sees the page drawn under the dashboard's strip, with no dashboard edit and no process
  started.
- Given a loaded page, the server sees one request from its frame, the document, and that
  request passes the origin guard unchanged.
- Given a page that calls a declared read-only tool with input, the result reaches the page
  and the call appears in the audit trail as a call to that tool.
- Given a page that calls a tool its manifest does not list under `tools`, a mutating tool
  through `call`, or another plugin's tool, the call is refused and nothing runs.
- Given a page script that fetches `/api/tasks`, reads the parent document, opens a window or
  adds a script element, each attempt fails in the browser.
- Given a bundle file changed on disk after install, the page is inactive with a diagnostic
  naming both digests, and the rest of the plugin still works.
- Given a manifest whose page names a mutating tool under `tools`, has no `index.html`, holds a
  `script` element in its markup, lists a script containing `</script`, or exceeds the size
  bound, `orbit plugin validate` refuses it by name. A same-plugin mutating tool under
  `actions` is accepted; under `tools` it is refused.
- Given another host selected, that host's page is written by this dashboard from the parts
  the tunnel returns, under the same frame policy.
- Given a plugin with panels and no pages, the Plugins tab behaves as it does today.
- Given a page that calls a consented action, the dashboard draws a confirmation outside the
  frame naming the plugin, the tool and the input; Run runs the tool once and Cancel runs
  nothing.
- Given an action request sent straight to the server without the operator session,
  without a confirmation, with one issued for another input, or replaying a used one, it
  is refused, nothing runs, and the request has an audit row with the web session.

## 10. Concerns & Honest Limitations

- **Browser measurements.** On 2026-11-27 the first contract, a directory of files served
  under `script-src 'self'`, failed in both engines tried ([ORB-29532]). WebKit blocked the
  separate bundle assets before requesting them. Chromium ran classic scripts in the fixture,
  but its subresource requests carried `Sec-Fetch-Site: cross-site`, which the Orbit router
  refuses. Module scripts and bundled fonts also failed their cross-origin checks.
- **The one-document probe passed in three engines.** Chromium 145.0.7632.6 and WebKit 26.0
  ran on macOS 27.2.1 arm64 with Playwright 1.58.2. Firefox 157.0 ran on Ubuntu 26.04.1 x64
  with Playwright 1.64.0 after pinned Firefox failed to launch on that Mac. The probe used
  the final [Serving](#2-serving) policy and the matching iframe sandbox in a loopback
  fixture. All three ran hash-admitted classic and module scripts, rendered inline styles,
  data images and a data font, and returned a bridge message whose origin was `null`. Each
  frame load reached the fixture as one same-origin navigation carrying its session cookie.
  A deliberate reload produced one further navigation. The frame sent no subresource or
  canary request. All three refused the attempted unlisted or later-added scripts, imports,
  `eval`, external assets, storage access, parent DOM access, fetch, form submission,
  popup and top navigation. Evidence and the probe are attached to [ORB-29532].
- **Navigation-boundary evidence.** On 2026-11-28, live browser verification ran against
  candidate `orbit web serve` in headless Chromium 156.0.8078.4 on Linux x64 ([ORB-29773]).
  The dashboard emitted `frame-src 'self'; child-src 'self'`, and the page route emitted
  `frame-src 'none'; object-src 'none'` while refusing direct top-level browser tab navigation
  with `403` `frame_required` (`sandbox; default-src 'none'`). Normal host-owned load,
  dashboard Reload, declared read-only `orbit.call`, mutating-tool refusal (`403 tool_refused`),
  `orbit.setQuery`, and same-document `#hash` navigation all succeeded. Eight navigation and
  nesting probes (`location.href`, `<a href>`, `<meta http-equiv="refresh">`, `data:text/html`,
  same-origin `/api/tasks`, `location.reload()`, `about:blank`, and nested `<iframe>`) produced
  zero requests to the external listener, tore down the frame and revoked the bridge on
  unsolicited frame navigation, and recovered cleanly via dashboard Reload. Same-origin
  self-navigation attempts may still issue a loopback request to the local Orbit server before
  hitting `403` or `frame-ancestors 'none'` and parent teardown; zero external requests leave
  the host. Firefox and WebKit binaries were not installed on this Linux host for a live
  re-run; their Fetch Metadata iframe headers were verified in [ORB-29532] (Firefox 157.0,
  WebKit 26.0), and CSP3 `frame-src` governs child frame navigations across all three engines.
  Evidence and screenshots are attached to [ORB-29773].
- **No file is fetched at run time.** A page that splits its code into chunks or loads an
  image by URL has to bundle them into its listed scripts and styles. `data:` URLs cost about
  a third more bytes than the files they carry, and the size bound counts the document as
  written.
- **A page can still mislead, and its plugin sees what the bridge carries.** It draws
  anything inside its frame, including a form that asks for a secret it then shows on screen.
  Frame isolation stops direct requests from the frame, but anything typed can still reach
  that plugin's backend as schema-valid input to a declared read-only tool through the
  [bridge](#3-the-bridge), which runs through audited tool dispatch. Treat anything typed
  into a page as visible to that plugin. The strip above the frame names which plugin's
  content it is; it does not keep operator input private from that plugin.
- **Each call starts the backend.** An `exec` backend is a process per call, so a page that
  polls every second costs a process every second. The in-flight sharing and the bound cap the
  damage and do not make polling cheap.
- **No push.** A page learns of a change by asking again.
- **A confirmation proves what the dashboard showed, not what the operator read.** The
  dialog names the tool and the input the server checked; a page can still describe an
  action misleadingly inside its frame before asking. The arm delay stops a click timed by
  the page, not a hurried operator. An action from another host's page is a `POST` through
  the forwarder like any other, and phase 2 adds nothing for it.
- **Another host's page needs an operator dashboard for its calls.** The forwarder refuses a
  `POST` without the operator session, and the call route is a `POST`; the call route has no
  exemption. On a dashboard without the operator session a remote page loads, its calls are
  refused, and the strip says a page on another host needs an operator dashboard. If that
  proves limiting, it goes to the operator as a decision; the forwarder is not widened for it.
- **Version skew.** A page written for bridge version 1 is served by whichever dashboard frames
  it. The manifest's `kit` number and the message's `v` are the contract, and a host that does
  not know a version refuses the page with a diagnostic.

## Task References

- [ORB-14679] — added `/api/on/<host>/…` forwarding to another host's dashboard API.
- [ORB-29504] — this design.
- [ORB-29534] — built the manifest key, validation, the bundle digest and the listings.
- [ORB-29536] — built the document and call routes and the kit.
- [ORB-29537] — built the Plugins sub-tabs, the sandboxed page frame, the strip and the bridge relay.
- [ORB-29538] — built the parts route and the forwarder writing another host's page from it.
- [ORB-29539] — built page actions behind consent and a dashboard-drawn confirmation.
- [ORB-29532] — measured the initial frame policy in Chromium and WebKit, then validated the one-document policy in Chromium, WebKit and Firefox.
- [ORB-29573] — revised the page contract to one document after that measurement.
- [ORB-29773] — enforced plugin page document navigation boundaries across dashboard CSP, Fetch Metadata iframe admission, and frame-controller teardown.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
