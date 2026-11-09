# UX rules

Runner for the CLI and UI rule sets. It is a hand-run QA and development tool, not a repository test and not a merge gate. The `ux-rules` routine that ran it on a schedule and filed tasks from its results was removed [ORB-22844], with its `step.mjs` script adapter and the rule-findings filer; the QA full sweep's `ux-measurement-browser` scenario still runs the rule tests.

A workspace that set up the routine still holds its script manifest, `.orbit/resources/scripts/ux_rules_runner.yaml`, whose entry, the deleted `step.mjs` adapter, no longer exists. Only jobs that reference this manifest refuse until it is repaired or removed. Delete the manifest, then run `orbit script revoke ux_rules_runner` to drop its grant. `orbit doctor` reports a leftover manifest under `retired-scripts`.

## Run

```sh
node scripts/ux-rules/run.mjs --surface cli --orbit /path/to/orbit --out /path/to/scratch
node scripts/ux-rules/run.mjs --surface cli --rule cli.help-alignment --orbit /path/to/orbit --out /path/to/scratch
. "${ORBIT_BROWSER_KIT_ENV:-$HOME/.local/chromium-deps/env.sh}"
node scripts/ux-rules/run.mjs --surface ui --base-url http://127.0.0.1:7878 --out /path/to/scratch
node scripts/ux-rules/run.mjs --surface ui --fixture --out /path/to/scratch
node scripts/ux-rules/run.mjs --surface ui --manifest website/visual/views.toml --base-url http://127.0.0.1:4321 --out /path/to/scratch
```

`--surface` is `cli` or `ui`. `--out` is the scratch directory the run may create. `--orbit` is required for `cli` and is the orbit binary under test. `--rule` limits the run to one rule id (the full id or its last segment).

Each CLI run calls `orbit init` in a fresh directory under `--out` and passes `--root` at that scratch root. It sets `HOME` inside that directory and does not point at the live Orbit store.

`--surface ui` needs exactly one of `--base-url` (live, read-only) or `--fixture`, loads `scripts/ux-rules/ui/`, and runs with the browser kit's pinned Playwright (`PLAYWRIGHT_MODULE` from the kit `env.sh`). `--manifest` may repeat; the default is `crates/orbit-web/visual/views.toml`.

The runner exits 0 when it has written `ux-rules.json`, including when individual results are `fail` or `error`. It exits non-zero when arguments are wrong, the binary is missing, no rules match, the manifest is invalid, or the report cannot be written.

## UI targets and the view manifest

A UI rule declares `targets: { views, widths, states }` (`views` is `"*"` or a list of view ids; `states` defaults to `["default"]`). The runner expands it over the manifest views × the rule's widths × each view app's themes × the declared states the view supports, opens one page per expanded target, and runs every rule that declared that target against it. A view with `responsive = false` gets only the first width.

Manifests are TOML in the view-manifest format of the archived visual-review design (§3.1): apps with `setup`, `themes`, `color_scheme_invariant`, `design_rules` and `tokens`; views with `id`, `app`, `title`, `route`, `fixture`, `ready`, `responsive`, `states`, `clip` and `mask`. `scripts/ux-rules/lib/manifest.mjs` validates at load: unknown apps or states, duplicate ids, and a view id containing a state word (`default`, `empty`, `loading`, `error`) or a theme word as a dot-separated part are refused, naming the id. A route may carry `{task}`-style placeholders, resolved from the fixture server's `ORBIT_VISUAL_<NAME>=` lines; in live mode an unresolved placeholder target is `skipped`.

Orbit's manifests are `crates/orbit-web/visual/views.toml` (dashboard, dark, `color_scheme_invariant`) and `website/visual/views.toml` (light and dark). Fixture seeds are JSON under `crates/orbit-web/visual/fixtures/`: `tasks` rows, and optional `notices` rows (`kind`, `title`, `body`) the setup script posts as open fleet notices through the CLI. Every top-level dashboard view needs a dashboard manifest entry or a recorded exemption; the orbit-web dashboard asset tests enforce it.

`--fixture` runs each app's `setup` per fixture (the `empty` state uses the `<fixture>:empty` seed) and reads `ORBIT_VISUAL_URL=` from its stdout (a plain loopback URL, as `astro preview` prints, is also accepted). `scripts/visual-fixture-server.sh <fixture>` seeds a disposable root and workspace from the fixture JSON and starts `ORBIT_VISUAL_ORBIT` (default `orbit` on `PATH`) as the API backend on port zero. A workspace front on `127.0.0.1` serves the admitted checkout's dashboard HTML, CSS, JS and fonts, including `/static/dashboard.css` joined in cascade order with the dashboard page policy. Requests under `/` and `/static/` are never forwarded to the backend: a declared asset that is missing or unreadable refuses startup, and any other path in that namespace is a fixture 404. Other requests proxy only to that backend's validated loopback origin. A network-path or absolute-form request target is refused before dispatch, so it cannot reach another host. An ordinary origin-form API path, including its query string, still reaches the backend. The experiments and implementers-scorecard overlays still answer their fixture JSON before that proxy. The backend and the front share the script's signal trap.

Readiness is the `ORBIT_VISUAL_URL` line, and only after those asset checks and a route-presence check. The front collects `/api` paths from the checkout's first-party dashboard scripts. A trailing-slash prefix is not a request. A template suffix that is not its own path segment, such as a query string, stays off the path. A `${}` path segment is probed as each checkout route (read from the orbit-web router source) that can serve that template: parameters become one fixed probe segment, and a literal segment is included only when that word appears as its own string in those scripts. A template that matches no checkout route is probed as written, so an unresolved path cannot pass silently. Each concrete path is probed with `OPTIONS` on the backend. An empty path set, a probe error, HTTP 404, or any status that does not show a present route refuses readiness and prints `source/API compatibility not established`. Payload schemas are not compared. The same stdout block records separate identities and does not claim they match: `ORBIT_VISUAL_SOURCE` (checkout `HEAD`), `ORBIT_VISUAL_RENDERER_SHA256` (the front script), `ORBIT_VISUAL_ASSETS_SHA256` (served checkout bytes), `ORBIT_VISUAL_BACKEND_SHA256` and `ORBIT_VISUAL_BACKEND_VERSION` (the backend file and its `--version` line), and `ORBIT_VISUAL_PROVENANCE` (a JSON file under the scratch directory with the same split and `static_backend_identity_compared` false). `ORBIT_VISUAL_TASK` and, for overlay fixtures, `ORBIT_VISUAL_EXPERIMENT` are unchanged. The ux-rules report `commit` stays the checkout `HEAD`. It names the source revision, not the backend binary.

When `faketime` is on PATH the server runs under it with its wall clock frozen at the fixture instant (`ORBIT_VISUAL_FROZEN_INSTANT`, default `2026-01-01T12:00:00Z`, passed to `faketime -f` as the UTC absolute timestamp `YYYY-MM-DD HH:MM:SS` with `TZ=UTC`): repeated wall-clock reads stay at that instant. The monotonic clock is not faked (`FAKETIME_DONT_FAKE_MONOTONIC=1`), so startup and readiness timers still advance in real time. This is a test-only seam of that script, not a shipped environment variable. Without it the browser clock is still frozen and seeded rows are created in one pass, so fingerprints stay stable while server-relative ages drift; the script logs `server_clock=faketime-absent`.

## UI context

`createUiContext` in `scripts/ux-rules/lib/ui-context.mjs` opens each target with the capture settings of the archived visual-review design (§4.2): viewport height 900 at 1440, 812 at 375, else `round(width × 10 / 16)`; device scale factor 1; `colorScheme` from the theme; `reducedMotion: reduce`; `UTC` and `en-US`; Playwright's clock fixed at the frozen instant `2026-01-01T12:00:00Z` (advanced one fixed 3s step so timer-deferred renders happen deterministically); animations, transitions and caret off; wait for `document.fonts.ready`, the view's `ready` selector, network idle and two animation frames; `mask` selectors painted flat. Requests are loopback-only — anything else is blocked and recorded — and every non-GET request is blocked: one attempted while a rule runs becomes that rule's `error`; one during page load is recorded for `console-clean`. The `loading` state holds same-origin API responses; `error` answers them 500.

A UI `check(ctx)` reads `ctx.page` and `ctx.target`, and returns results built with `uiResultOf` (or the `verdict` helper: no matched element is `skipped`, never a silent pass). `measureInPage` runs a measurement in the page with shared helpers. Each `fail` gets a screenshot under `evidence/<rule id>/<view>-<theme>-<width>-<state>.png`. When the result identifies the measured element — a failure's `evidence_selector` (a unique handle a rule stamps on the node, as `ui.cramped-wrap` does, because its plain selector is a tag plus classes shared by many nodes), or its `selector` disambiguated by the measured `text` and `width_px` — the capture scrolls that element so its start sits in the viewport with the surrounding rows and card in frame, and captures the viewport instead of the manifest `clip`. That clip is chosen for whole-view captures, so it can omit the measured text or stretch it into an unreadable strip. The capture restores every scroll position it changed — initially-zero nested offsets included, on the framed and fallback paths alike — and removes every temporary handle before returning (only rows inside the 20-row result cap are ever stamped), so later rules measuring the same page see the state the load left. A result that names a measured element but whose element does not resolve to exactly one node, or whose framed viewport capture fails, never falls back to the manifest clip: that clip can omit the measured text, so presenting it as this finding's screenshot would be misleading. Instead the capture deletes any partial or stale file at the destination, restores scroll and handles, and throws an explicit `evidence_capture_failed` diagnostic; the runner retains it in `measurements.evidence_capture` and attaches an `evidence-capture-failed` text file as the result's only capture evidence, with no PNG. The underlying finding, its fingerprint, severity and cap/dedup semantics are unchanged. Only a genuinely untargeted result — one that names no measured element at all — keeps the previous behaviour: the view's `clip` selector when set, otherwise the viewport. The fingerprint is the stable aggregate identity `rule id|view|theme|width|state`, never a measurement or a selector, so the same rule/target keeps one fingerprint when it fails, when a different element fails first, and when it passes again. Specific failing selectors and geometry stay in `measurements` (`measurements.failures` for `verdict` results, `measurements.selector` when a rule passes an explicit selector to `uiResultOf`).

## Writing ad-hoc captures

`openTarget` freezes Playwright's clock with `page.clock.pauseAt` at `lib/ui-context.mjs:328`. A paused clock also stops `requestAnimationFrame`, so an unbounded `page.evaluate` that waits for animation frames can hang. The helper bounds its two-frame wait with a 2-second host-side timer at `lib/ui-context.mjs:353-361`; the element-framing capture wait is bounded the same way at `lib/ui-context.mjs:553-561`. Use a host-side deadline for your own frame waits.

Bound every awaited Playwright page call. Use the API's `timeout` option where available; for calls such as `page.evaluate` or `page.clock.runFor` without one, race the promise against a Node-side timer. A timeout race does not cancel the browser operation, so discard that page if the deadline expires.

After `openTarget`, `/api/log/stream` is a long-lived SSE request. A broad `page.route("**/api/**", route => route.fetch())` also matches that stream, and `route.fetch()` waits for it to finish. Route only the endpoint under test, for example `**/api/tasks/**`, so the stream is left alone.

One `page.clock.runFor(...)` call hung after a task-detail disclosure changed the DOM, but the hang was not reproduced in isolation and its cause is unconfirmed. Keep any `runFor` call behind a host-side deadline; when waiting for the updated UI, prefer a specific locator wait with a timeout.

## UI rules

| Id | Measures |
|---|---|
| `ui.no-horizontal-overflow` | Document and active pane `scrollWidth <= clientWidth + 1` at 1440, 1166 and 375px; bounded overflow wrappers exempt. |
| `ui.panel-body-padding` | Each panel body's content starts with a non-zero inset (body padding, or the first painted content element's offset and padding — a summary table's first cell counts; an assistive live region clipped to zero area, such as `.panel-placeholder.quiet`, is skipped), and is not far closer than its sibling panels'. Catches the Lifecycle diagnostics card, whose counts sit in a zero-padding body while sibling cards start at 6–12px. |
| `ui.sibling-spacing` | Siblings in one container share padding, font size and weight only when they are the same documented semantic kind. A tag plus the first class is a candidate cluster, not identity. Experiment badges (`.ex-badge`) document three kinds — status (`.ex-status-*`), mode (`.ex-mode-*`) and verdict (`.ex-verdict` / `.ex-verdict-*`) — so a verdict's font-weight 600 is not a mismatch against an adjacent status or mode badge; different status values of one kind are still peers. A homogeneous group that disagrees still fails, naming the node (`selector`), the property (`padding`, `font_size` or `font_weight`), the value and the sibling value. Selected state is exempt and is not a kind: `aria-selected="true"`, `aria-pressed="true"`, any `aria-current`, or a whole class token `active` or `selected`. A class that only contains those words (`active-triage-chip`) and the token `on` are not selected. A cluster that shares a shell but diverges by an undocumented class is `ambiguous`: it is recorded under `measurements.coverage` and is not failed, and other groups on the view are still measured. A documented kind with fewer than three peers is `distinct-kind` and is not compared across kinds. |
| `ui.control-size` | Buttons in one toolbar or row share height (within 2px) and font size. |
| `ui.aligned-starts` | Neighbouring grid or flex-row columns whose border boxes share a top (within 4px) have comparable content starts within 8px. The start is the first visible text line, so a column's own text before a nested child counts and a later wrapped line does not. A button, field or replaced graphic contributes its own start. A single-line input or closed select uses its centred line. A list select uses its first line. A textarea uses the first line wholly inside its content clip, so scrolling by a whole number of lines leaves its start at the content top and a partial-line scroll displaces that line by the remainder; it never measures the original first line hidden above the clip. A scrolled textarea whose used line height cannot be established (for example `line-height: normal`), or whose line otherwise cannot be measured, is recorded as unavailable and is not counted as a pass. Text is compared only with text, and controls only with controls. A clipped assistive box is not a start, and neither is text whose clip-path paints no area (an inset that covers the box on an axis, or a zero-radius circle or ellipse) or that sits inside such a clipped ancestor. An anchor with no peer of its kind, no rendered content or no geometry is recorded and is not a pass. A failure names the measured anchor, its peer and the delta. |
| `ui.even-row-panels` | Panels side by side in one row have heights within 4px. |
| `ui.bounded-length` | Page `scrollHeight` at most 12 viewports; lists over 150 rows need a pagination control. |
| `ui.nested-card` | No bordered or filled box directly inside a bordered panel without a grouping role or label. The same check covers a task-detail disclosure row (`.detail-disclosures > .field-block`, including a row whose only border is the group's), the description's `.task-markdown-panel`, and the `.task-delivery-card`. Task detail renders no `.panel`, so those containers are what make that view measured. A full-bleed band is not a card: its border box is flush with the container content box (within 1px), every margin on all four sides is within 1px of 0 (a negative margin that overlaps a neighbour is a separating margin and fails), it is in normal flow (`static` or `relative`, and not floated; a float on a flex or grid item does not apply, so that item stays in flow), and it has no corner radius, shadow, outline or left/right border. A top or bottom hairline is a section divider between bands. An inset fill, a separating margin, a side border, a radius, a shadow, an outline or an out-of-flow box still fails, including when that box is full width. The fingerprint stays `ui.nested-card\|view\|theme\|width\|state`. |
| `ui.contrast` | Visible text at least 4.5:1 (3:1 large) against its effective background; gradient-backed text is recorded unmeasurable. Text inside a natively disabled control (`button`, `input`, `select`, `textarea`, `optgroup` or `option` matching `:disabled`, which includes controls disabled by a disabled fieldset outside its first legend) is exempt and counted as `disabled_exempt`. `aria-disabled`, colour tokens and cosmetic classes do not exempt. |
| `ui.accessible-name` | Visible buttons, links, inputs, selects, textareas and images have a name (text, `aria-label`/`aria-labelledby`, label, `alt`, or title); decorative images (`alt=""`) pass. |
| `ui.console-clean` | No page errors, console errors, failed same-origin requests, off-loopback requests, or blocked non-GET requests while rendering. |
| `ui.forecast-unavailable-reason` | API-shaped fixture rule: when `GET /api/diagnostics/backlog_forecast` (fetched in the page's workspace scope) reports unavailable reasons, the view carries `data-forecast-reason` with a known kind (`regression_history_missing`, `risk_unestimated`, `arrival_conflict_unknown`, …) and visible reason text. |
| `ui.text-occlusion` | Text sampled inside its own glyph line boxes is not covered where an element painted above it actually paints: its own glyph boxes contain the sample, or an opaque fill or replaced content covers it. The paint stack is read with pointer events forced on, so an opaque `pointer-events: none` overlay counts and a transparent box whose glyphs paint elsewhere does not. A sticky or fixed bar is not coverage when the text's own scrollport can move the glyphs clear of it; an overlay that stays over the glyphs at every scroll offset still fails. |
| `ui.text-overflow` | Content wider than its container is not cut by a clipping ancestor or run into a sibling; bounded scroll wrappers, `text-overflow: ellipsis`, screen-reader-only 1px boxes and collapsed boxes are exempt. |
| `ui.cramped-wrap` | A 2–5 word label (up to 60 characters) does not stack to 3+ lines at ≤1.5 words per line; longer text must not occupy 9+ lines averaging under 25 characters per line. Visible text extending from a collapsed zero-width box also fails; transparent and zero-width clipped holders are excluded. These checks exclude table cells and compact controls such as pills, chips and badges. |
| `ui.cramped-padding` | Own-text glyphs keep at least a 4px inset (within a pixel for subpixel noise) from the nearest bordered or filled block-level container's border box, on sides with a drawn edge (border, padding or a differing fill); pills, chips, buttons, table parts and absolutely positioned runs are out of scope. |
| `ui.tight-leading` | Multi-line text (2+ line boxes) has a numeric `line-height` of at least 1.3× its font size; `normal` is recorded, never failed. |
| `ui.undersized-text` | Content tags (`p`, `li`, `td`, `th`, `dd`, `dt`, buttons, links, labels, headings) meet the 11px dashboard minimum (the theme's smallest named content size, 11px column heads); only uppercase text with letter-spacing of at least 5% of the font size (a tracked overline) is exempt; either style alone is ordinary prose. |
| `ui.heading-rhythm` | Rendered space above a section heading (nearest block above through the ancestor chain, plus the larger adjoining margin when the boxes touch) is at least the space below it. |

## Rule module

Each file in `scripts/ux-rules/<surface>/` is one rule:

```js
export const rule = {
  id: "cli.help-alignment",
  title: "Grouped help descriptions start in one column",
  severity: "medium", // critical | high | medium | low | nit
  surface: "cli",
  targets: [{ id: "orbit run", argv: ["run"] }], // optional
};

export async function check(ctx) {
  return []; // one result object, or an array
}
```

`check` may return the objects produced by `resultOf` from `scripts/ux-rules/lib/cli-context.mjs`. A throw becomes one `error` result and the runner continues with the next rule. A rule that declares `targets: []`, or that returns no results, is `error` (it matched no target). It is never a silent skip.

Omit `targets` when the rule discovers its own targets (every command, every MCP tool) and returns one result per target.

## CLI context

`createCliContext` is what `--surface cli` passes as `ctx`.

- `ctx.capture({ argv, width, json, fresh, timeout })` runs `orbit --root <scratch> ...argv`. `width: null` (the default) is a pipe, and the recorded renderer width is 0. A numeric `width` is a PTY: the capture clears `COLUMNS`, sets the terminal size, and records the width `TIOCGWINSZ` reads back. That ioctl width is the renderer width. `COLUMNS` alone is not.
- `ctx.discoverCommands()` lists argv arrays for the binary's command tree, starting at `[]` for `orbit` itself. Only grouped-help and `Commands:` rows whose name is a single subcommand token are followed. Example blocks are not commands.
- `ctx.listMcpTools()` starts `orbit mcp serve` on the scratch root, completes `initialize` and `notifications/initialized`, then reads `tools/list`.
- `ctx.host` is the scratch machine id. `ctx.commit` is `git rev-parse HEAD` of the repo the runner was started in.
- `ctx.writeEvidence(ruleId, targetId, label, text)` writes a file under `<out>/evidence/` and returns the relative path.

## Results

`<out>/ux-rules.json`:

```json
{
  "schema_version": 1,
  "surface": "cli",
  "commit": "<git sha>",
  "host": "hm_<scratch>",
  "target": { "mode": "scratch", "orbit": "<absolute binary>" },
  "results": [
    {
      "rule": "cli.help-alignment",
      "target": { "id": "orbit run", "argv": ["run"] },
      "status": "pass",
      "severity": "medium",
      "measurements": {},
      "evidence": ["evidence/cli.help-alignment/orbit_run-widths.txt"],
      "fingerprint": "cli.help-alignment|orbit run|run"
    }
  ],
  "summary": { "pass": 0, "fail": 0, "error": 0, "skipped": 0 }
}
```

`status` is `pass`, `fail`, `error`, or `skipped`. `fail` is a product finding. `error` means the rule threw, returned nothing, or declared no targets. `skipped` means the rule named a target it could not measure (for example a command with no `-h`, or an MCP tool with no CLI command).

The fingerprint is `rule id`, the target id, and the target argv (or the target id when argv is empty). It never includes a measurement, a width, a byte count, or an absolute path. A target keeps the same fingerprint when it passes or fails.

## CLI rules

| Id | Measures |
|---|---|
| `cli.help-alignment` | Grouped help at PTY widths 60, 80 and 120, and piped. Sibling descriptions share one column with a gap of at least two after the longest name. Example blocks are not grouped help. The evidence records each requested width and the ioctl width. |
| `cli.help-hanging-indent` | Wrapped descriptions in that grouped help keep one hanging indent, and a line break is not inserted while the next word would still fit. |
| `cli.short-help-shorter` | Where both exist, `-h` carries no content that `--help` lacks. It compares the forms' words (lowercased, punctuation dropped, counted with repeats), not bytes, and ignores clap's help-flag footer: clap pads `-h` options into aligned columns, so a short help can have more bytes and still say less. A word in `-h` beyond `--help` fails, with a sample in `missing_sample`. When `--help` adds no words, the target is `skipped` with reason `no long help`. |
| `cli.help-length` | Each flag's prose is one line. Clap's `Possible values:` list is not prose. Long help is at most 8192 bytes. |
| `cli.json-valid` | `task list`, `workspace list`, and `config show` `--json` parse, and a second call keeps the same top-level shape. |
| `cli.error-exit-codes` | An unknown task id, an unknown flag, and conflicting flags exit non-zero and name a next step. |
| `cli.mcp-description-length` | Tool descriptions at most 1000 bytes. Field descriptions at most 800 bytes. |
| `cli.mcp-cli-parity` | A tool and its CLI command agree on required inputs. CLI-only provenance flags (`--model`) and a refused `agent` input are ignored. A required MCP field that the CLI exposes as a global option, including `workspace`, is not a mismatch. For `auto-task add` only, `schedule` is `--cron`, `--every-minutes`, or `--deliveries-landed` (the usage line cannot show that one-of group), and `template` is required `--title` when the tool describes `title` as a required template member. Other commands are compared by field name. Defaults are compared only when both sides state one. A tool with no CLI command is `skipped`. |

## Report, owner context and raw archive

A full UI sweep measures tens of thousands of member observations. `ux-rules.json` is therefore a compact report in the filing format, and the complete raw report is archived beside it. The filing report holds every `fail`, `error` and `skipped` observation; every observation, passing ones included, of a defect group that fails anywhere in this run or that an open owner tracks; every result that declares no members (CLI results, runner errors); and one compact record per expanded aggregate target that did not fail, under its aggregate fingerprint, so a legacy aggregate owner keeps its pass semantics. An aggregate whose members include a non-passing observation (an incomplete console capture, say) gets no aggregate pass record. A failure with no stable cause identity (an element with no unique id, `data-key` or `data-testid` anchor) is not reported as its own member: its aggregate is reported as `fail` under the aggregate fingerprint with `measurements.ambiguous_failures`, so a legacy aggregate owner keeps receiving it and no task is minted from a DOM-order occurrence. Untracked passing members are counted in `summary.omitted_untracked_passing`, never listed. Filed measurements keep scalar values and small objects; larger shared values are named under `measurements.archived`. Console measurements use port-normalized endpoints and sources; the actual URLs stay under `provenance`, which only the archive keeps, so repeated fixture runs file identical measurements.

`summary` counts every raw observation by status, plus `observations`, `defect_groups`, `reported`, `reported_aggregates`, `omitted_untracked_passing`, `ambiguous_failures`, `absence_credits`, `tracked_groups` and `tracked_groups_unobserved`. `archive` describes `archive/observations.partNN.gz`: one gzip stream of the compact raw report (every expanded member and every aggregate result, with full measurements and its declared observations before member deduplication, so repeated failures of one identity are kept), split into parts of at most 1 MiB. Concatenating the parts in order and inflating them reproduces the bytes `uncompressed_sha256` names; the descriptor also records `uncompressed_bytes`, `gzip_bytes`, `gzip_sha256`, `observation_count`, `aggregate_count` and each part's `bytes` and `sha256`.

`--owner-context <file>` reads an owner context, the shape the removed `prepare_rule_findings` activity returned: `{schema_version, groups: [{surface, rule, defect: {version, key}, members}]}` for the groups open owners track, where `members` lists the aggregate fingerprints the group's observations were recorded under. Their observations are reported in full, so a tracked group can be found repaired only when every known member is measured passing. A console or page-error cause is never re-observed once fixed, so when a page's console scope completes with nothing but passes, each tracked group member last recorded under that aggregate gets a pass with its own identity (`measurements.observation.cause_absent`, counted in `summary.absence_credits`). Element rules make no such absence claim: a vanished element earns no credit.

### Console diagnostics

Failures and successes are kept apart. Failed responses keep method, endpoint, status, failure class and the initiator stack; failed requests keep their initiator. A response with status below 400 is never a diagnostic: it is a passing observation of its request cause, which is how a repaired endpoint credits a known group. Successes are deduplicated by method, URL and initiator in their own list of at most 100 per page (`counts.successful_responses`); further ones are counted in `counts.successful_responses_omitted` and only withhold credit from the causes they would have repaired. Request contexts are retired when the request finishes, so static resources cannot exhaust the bounded lists. More than 100 requests in flight, or more than 100 failures of one kind, mark the list dropped. That is a real coverage gap: the rule records a `diagnostic-limit` error observation, and that target earns no pass credit.

## Handoff: ux-review-cli auto-task update

This change does not modify `ux-review-cli`. The definition is operator configuration, and this lane is not granted `orbit.auto_task` action `update`. The resumed implementation step must copy this handoff into the task execution summary. Recovery does not write that summary.

Apply it on the owner workspace with a field-scoped patch. Do not send a whole `template`: omitted template fields receive defaults. Do not change `enabled`, the schedule (`every_minutes` 4320), complexity (`hard`), tags, priority, or `required_tools`.

The patch drops the description sections "Help alignment and wrapping regression checks" and "Diagnostic summary spacing and cross-surface findings", and it drops the two acceptance criteria that re-measure grouped help and diagnostic summaries. The Lifecycle diagnostics card stays a dashboard finding for `ux-review-ui`.

```bash
orbit tool run orbit.auto_task --input-file ux-review-cli-template-patch.json
```

`ux-review-cli-template-patch.json`:

```json
{
  "action": "update",
  "name": "ux-review-cli",
  "workspace": "ws_orbit",
  "template_patch": {
    "description": "Review the user experience of Orbit's two programmatic surfaces: the `orbit` CLI, for operators, and the MCP server (`orbit mcp serve`, the `orbit.*` tools), for agents.\nCompare what you find with the previous pass, and file one Orbit task for each evidence-backed problem. This task files work; it does not change any command or tool itself. Its durable outputs are the review artifact and the filed tasks. A pass may legitimately finish with no new tasks, when nothing clears the bar below.\n\n## Safety\n\n- **Read-only against the live store.** Run read commands freely: `list`, `show`, `history`, `search`, `readiness`, `doctor`, `config show` and `--help`.\n- **Mutations only in a throwaway workspace.** To exercise a mutating command or tool (task add, update, approve, auto-task, run flows), use a fresh `orbit init` under `$ORBIT_SCRATCH_DIR` with `--root` pointing at a scratch Orbit root. Never touch the live store, a live drain or `orbit-web.service`.\n- **Mistakes on purpose, also in scratch.** Error paths (bad IDs, wrong flags, conflicting options, missing workspace, refused transitions) are part of the review. Trigger them only in the scratch setup.\n- **Disk gate:** before creating any worktree, build dir or scratch root, check `df --output=pcent <parent-dir> | tail -1`. At 80% or more, create nothing new and record the reading in the summary.\n\n## Run the CLI rules first\n\nBefore any judgement, run `node scripts/ux-rules/run.mjs --surface cli --orbit <binary under review> --out <scratch>` against a scratch root. Read `<scratch>/ux-rules.json`. Treat every `fail` as known: do not re-measure it and do not file a second task for it. A rule `error` is a broken rule, not a product finding. The rules already cover grouped-help alignment, hanging indent, help length, short help versus long help, JSON shape, error exit codes, MCP description length, and CLI/MCP required-input parity.\n\n## What to review\n\n1. **CLI.** Walk the commands operators use daily: `task` (add, list, show, update, settle-merged), `run` (auto, ship, show, history, readiness), `auto-task`, `workspace`, `doctor`, `config`, `browser-kit`, `web`, `agents` and `search`. Look at wording, dead ends, and whether help matches behaviour. Do not re-measure what a rule covers.\n2. **MCP.** Start `orbit mcp serve` in the scratch workspace and call the tools agents use most against the scratch store: `orbit.task.*`, `orbit.search`, `orbit.task.artifact.*`, `orbit.agents.*` and run inspection. Judge choice, result shape, and refusal text. Do not re-rank description length or re-check required-input parity; the rules did that.\n\nWhat counts as a finding:\n\n- **Broken:** a command or tool that fails on valid input, gives a wrong exit code, produces `--json` that isn't valid JSON or changes shape between calls, or makes a documented example fail, and that no rule already recorded.\n- **Dead end:** an error or refusal that doesn't say what to do next, or a state that no command can get out of, beyond the cases the error-exit rule already records.\n- **Clarity:** help text that is vague, contradicts behaviour, or exposes internal task or friction IDs; unexplained jargon; output without units or context.\n- **Verbosity:** help or descriptions that bury what the reader needs, beyond the length rules. The fix moves the detail into the docs (it is not deleted) and leaves a pointer.\n- **Consistency:** flags, nouns or verbs that differ for the same concept across commands; output formats that drift; a CLI/MCP disagreement the parity rule did not record.\n- **Discoverability:** a common workflow that needs an undocumented flag, or a feature reachable from only one surface without a reason.\n\nIgnore pure taste. Every finding needs a reproduction command, with the scratch `--root` if it mutates, the actual output, the expected behaviour, and a code pointer.\n\n## Compare with the previous pass\n\nFind the previous pass: `orbit tool run orbit.task.list` with `{\"status\":[\"done\",\"review\"],\"tag\":\"ux-review-cli\",\"limit\":5}`. Read its `ux-review.json` artifact. Mark each earlier finding as fixed, still present, or regressed. The first pass establishes the baseline.\n\n## Duplicate safety\n\nBefore filing:\n- Check the `surface-parity-sweep` auto-task's open work. It owns structural CLI, MCP and HTTP duplication, so file only the user-facing side of a parity issue.\n- List open work with `orbit.task.list` (statuses proposed, backlog, in-progress, review and blocked; tags `cli`, `mcp` and `ux-review`).\n- Search for the command or tool name with `orbit.search`.\n- If an open task already covers a finding, comment on it with the new evidence instead of filing a second one.\n\n## Filing\n\n- **Scope:** one task per root cause. A shared error helper that produces five bad messages is one task.\n- **Limit:** file at most 8 tasks per pass. Rank by impact (broken, then dead end, then clarity, then verbosity, then consistency, then discoverability) and list the rest in the artifact. One verbosity task may cover several commands that share a cause.\n- **Fields:**\n  - `type` is `bug` for broken findings and `feature` otherwise.\n  - Tags are `[\"ux-review\", \"cli\"]` or `[\"ux-review\", \"mcp\"]`, or both.\n  - Complexity is low or medium, or hard when the fix crosses crates or changes a persisted or external contract.\n  - `context_files` carry verified `file:` selectors.\n- **Description:** the reproduction command, the actual output, the expected behaviour, the user or agent impact, and code pointers.\n- **Acceptance criteria:** observable through the same command or tool call; never pin exact wording; note when a golden (CLI help or MCP text) must be regenerated.\n- Tasks land in `proposed`. Don't approve them.\n\n## Handoff\n\n- Store the full findings table as the artifact `ux-review.json` with `orbit tool run orbit.task.artifact` action `put`. Each row records surface, command or tool, category and severity; the reproduction; disposition (filed id, commented-onto id, or deferred); and status against the previous pass.\n- The execution summary covers the commands and tools reviewed, every filed or commented finding, the deferred findings, and anything you couldn't exercise and why.\n- The Lifecycle diagnostics card is a dashboard layout finding covered by `ux-review-ui`. Refer matching dashboard findings there. Do not infer a CLI defect from that screenshot.\n",
    "acceptance_criteria": [
      "The pass ran `node scripts/ux-rules/run.mjs --surface cli` against a scratch root before any judgement, read ux-rules.json, and treated every fail as known.",
      "Daily-use CLI and MCP judgement findings only. Grouped-help alignment, hanging indent, help length, short-help length, JSON shape, error exit codes, MCP description length, and CLI/MCP parity were not re-measured by hand. Anything not exercised is named.",
      "The findings table is stored as the task artifact ux-review.json and compared with the previous ux-review-cli pass (or marked as the baseline).",
      "Each finding that clears the bar is filed as one proposed task per root cause (at most 8, ranked by impact) with a reproduction command, actual vs expected output and code pointers, or commented onto the open task that already covers it; the rest are listed as deferred.",
      "The live store, drains and services were not mutated.",
      "A pass with no qualifying findings finishes successfully with the artifact stored and no task filed.",
      "The Lifecycle diagnostics card remains a dashboard finding for ux-review-ui. No CLI defect is inferred from that screenshot."
    ]
  }
}
```

## Handoff: ux-review-ui auto-task update

This change does not modify `ux-review-ui`. The definition is operator configuration, and this lane is not granted `orbit.auto_task` action `update`. Apply the generated patch — attached to the delivering task as `ux-review-ui-template-patch.json` — on the owner workspace with a field-scoped patch. Do not send a whole `template`: omitted template fields receive defaults. Do not change `enabled`, the schedule (`every_minutes` 4320), complexity (`hard`), tags, priority, or `required_tools`.

```bash
orbit tool run orbit.auto_task --input-file ux-review-ui-template-patch.json
```

The patch file must carry `"action": "update"` beside `name`; add it when the attached copy lacks it.

The patch is `{"name": "ux-review-ui", "workspace": "ws_orbit", "template_patch": {"description": …, "acceptance_criteria": […]}}`, derived from the live definition at delivery time. It:

- adds a "Run the UI rules first" section: run `--surface ui` against the live dashboard (and the website manifest against the preview) before any judgement, read `ux-rules.json`, treat every `fail` as known, and use `--fixture` for the forecast-reason rule when live data has no unavailable forecast;
- replaces the visual-design measurement list (`getBoundingClientRect()` / `getComputedStyle()` sibling comparisons and `scrollHeight` checks) with the judgement remainder — misplaced affordances and wasted space — and routes a measurement-shaped judgement finding to a `ux-rules` task that adds the rule;
- deletes the three 2026-10-29 regression sections ("Viewport containment regression check", "Panel body padding regression check", "Backlog-forecast unavailable reasons and qualified baseline") and their three acceptance criteria, now covered by `ui.no-horizontal-overflow`, `ui.panel-body-padding` and `ui.forecast-unavailable-reason`;
- re-scopes the visual-design acceptance criterion to judgement findings read against `ux-rules.json`, and prepends a criterion requiring the runner-first pass.

To regenerate it after the definition drifts: read the current template with `orbit.auto_task` action `list`, apply the same four edits, and keep everything else byte-identical.
