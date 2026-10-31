# UX rules

Runner for the CLI and UI rule sets. The runner is a scheduled measurement, not a repository test and not a merge gate.

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

Manifests are TOML in the visual-review §3.1 format: apps with `setup`, `themes`, `color_scheme_invariant`, `design_rules` and `tokens`; views with `id`, `app`, `title`, `route`, `fixture`, `ready`, `responsive`, `states`, `clip` and `mask`. `scripts/ux-rules/lib/manifest.mjs` validates at load: unknown apps or states, duplicate ids, and a view id containing a state word (`default`, `empty`, `loading`, `error`) or a theme word as a dot-separated part are refused, naming the id. A route may carry `{task}`-style placeholders, resolved from the fixture server's `ORBIT_VISUAL_<NAME>=` lines; in live mode an unresolved placeholder target is `skipped`.

Orbit's manifests are `crates/orbit-web/visual/views.toml` (dashboard, dark, `color_scheme_invariant`) and `website/visual/views.toml` (light and dark). Fixture seeds are JSON under `crates/orbit-web/visual/fixtures/`.

`--fixture` runs each app's `setup` per fixture (the `empty` state uses the `<fixture>:empty` seed) and reads `ORBIT_VISUAL_URL=` from its stdout (a plain loopback URL, as `astro preview` prints, is also accepted). `scripts/visual-fixture-server.sh <fixture>` seeds a disposable root and workspace from the fixture JSON, serves the real dashboard on port zero (`ORBIT_VISUAL_ORBIT` picks the binary), and prints `ORBIT_VISUAL_URL` and `ORBIT_VISUAL_TASK`. When `faketime` is on PATH the server runs under it with its wall clock frozen at the fixture instant (`ORBIT_VISUAL_FROZEN_INSTANT`, default `2026-01-01T12:00:00Z`, passed to `faketime -f` as the UTC absolute timestamp `YYYY-MM-DD HH:MM:SS` with `TZ=UTC`): repeated wall-clock reads stay at that instant. The monotonic clock is not faked (`FAKETIME_DONT_FAKE_MONOTONIC=1`), so startup and readiness timers still advance in real time. This is a test-only seam of that script, not a shipped environment variable. Without it the browser clock is still frozen and seeded rows are created in one pass, so fingerprints stay stable while server-relative ages drift; the script logs `server_clock=faketime-absent`.

## UI context

`createUiContext` in `scripts/ux-rules/lib/ui-context.mjs` opens each target with the visual-review §4.2 settings: viewport height 900 at 1440, 812 at 375, else `round(width × 10 / 16)`; device scale factor 1; `colorScheme` from the theme; `reducedMotion: reduce`; `UTC` and `en-US`; Playwright's clock fixed at the frozen instant `2026-01-01T12:00:00Z` (advanced one fixed 3s step so timer-deferred renders happen deterministically); animations, transitions and caret off; wait for `document.fonts.ready`, the view's `ready` selector, network idle and two animation frames; `mask` selectors painted flat. Requests are loopback-only — anything else is blocked and recorded — and every non-GET request is blocked: one attempted while a rule runs becomes that rule's `error`; one during page load is recorded for `console-clean`. The `loading` state holds same-origin API responses; `error` answers them 500.

A UI `check(ctx)` reads `ctx.page` and `ctx.target`, and returns results built with `uiResultOf` (or the `verdict` helper: no matched element is `skipped`, never a silent pass). `measureInPage` runs a measurement in the page with shared helpers. Each `fail` gets a viewport screenshot under `evidence/<rule id>/<view>-<theme>-<width>-<state>.png` (the view's `clip` selector when set). The fingerprint is `rule id|view|theme|width|state|selector`, never a measurement, so two fixture runs on an unchanged build produce identical fingerprints.

## UI rules

| Id | Measures |
|---|---|
| `ui.no-horizontal-overflow` | Document and active pane `scrollWidth <= clientWidth + 1` at 1440, 1166 and 375px; bounded overflow wrappers exempt. |
| `ui.panel-body-padding` | Each panel body's content starts with a non-zero inset (body padding, or the first content element's offset and padding — a summary table's first cell counts), and is not far closer than its sibling panels'. Catches the Lifecycle diagnostics card, whose counts sit in a zero-padding body while sibling cards start at 6–12px. |
| `ui.sibling-spacing` | Siblings of one kind in one container share padding, font size and weight; selected/active items are exempt. |
| `ui.control-size` | Buttons in one toolbar or row share height (within 2px) and font size. |
| `ui.aligned-starts` | First content of neighbouring grid/flex-row columns starts within 8px of the same top. |
| `ui.even-row-panels` | Panels side by side in one row have heights within 4px. |
| `ui.bounded-length` | Page `scrollHeight` at most 12 viewports; lists over 150 rows need a pagination control. |
| `ui.nested-card` | No bordered or filled box directly inside a bordered panel without a grouping role or label. |
| `ui.contrast` | Visible text at least 4.5:1 (3:1 large) against its effective background; gradient-backed text is recorded unmeasurable. |
| `ui.accessible-name` | Visible buttons, links, inputs, selects, textareas and images have a name (text, `aria-label`/`aria-labelledby`, label, `alt`, or title); decorative images (`alt=""`) pass. |
| `ui.console-clean` | No page errors, console errors, failed same-origin requests, off-loopback requests, or blocked non-GET requests while rendering. |
| `ui.forecast-unavailable-reason` | API-shaped fixture rule: when `GET /api/diagnostics/backlog_forecast` (fetched in the page's workspace scope) reports unavailable reasons, the view carries `data-forecast-reason` with a known kind (`regression_history_missing`, `risk_unestimated`, `arrival_conflict_unknown`, …) and visible reason text. |

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
| `cli.short-help-shorter` | Where both exist, `-h` is shorter than `--help`. |
| `cli.help-length` | Each flag's prose is one line. Clap's `Possible values:` list is not prose. Long help is at most 8192 bytes. |
| `cli.json-valid` | `task list`, `workspace list`, and `config show` `--json` parse, and a second call keeps the same top-level shape. |
| `cli.error-exit-codes` | An unknown task id, an unknown flag, and conflicting flags exit non-zero and name a next step. |
| `cli.mcp-description-length` | Tool descriptions at most 1000 bytes. Field descriptions at most 800 bytes. |
| `cli.mcp-cli-parity` | A tool and its CLI command agree on required inputs. `model` and `agent` are provenance on both sides and are ignored. A required MCP field that the CLI exposes as a global option, including `workspace`, is not a mismatch. Defaults are compared only when both sides state one. A tool with no CLI command is `skipped`. |

## Filing and the routine

The deterministic `file_rule_findings` activity turns one results document into tasks: one proposed bug task per new failing fingerprint (the fingerprint is stored in the task's `rule_fingerprint` field, with the measurements, the evidence and a reproduce command naming `--rule` and the target), a comment on the open owner when measurements change, exactly one comment when the fingerprint passes again (never a status change), and one task per rule for `error` results, tagged `ux-rules-error`. New tasks per run are capped by `ux_rules.max_new_tasks` (default 10, highest severity first); results below `ux_rules.file_at` (default `low`) or for rules in `ux_rules.disabled` are recorded in the run output only. The filer is generic: any script that writes this schema can feed it.

`ux_rules_pipeline` pairs a granted script step with the filer, and the seeded `ux-rules` routine runs it daily (disabled until enabled). The script step names a workspace script `ux_rules_runner`: an operator writes a Script manifest at `.orbit/resources/scripts/ux_rules_runner.yaml` whose entry runs this runner once per surface in `ux_rules.surfaces` and prints ONE merged results document as its stdout JSON, then grants it with `orbit script grant ux_rules_runner`. Without the grant the step refuses before spawn; there is no shell fallback. A network-`none` script step cannot reach a dashboard on the host's loopback (its own network namespace), so a live `--base-url` UI surface waits for script steps' network phase; `--surface cli` and `--fixture` UI runs (the fixture server is inside the script's namespace) work as-is. `orbit run show` for a routine run carries the pass/fail/error/skipped counts and the tasks filed or commented in the filing step's output.

## Handoff: ux-review auto-tasks after the routine (phase 3)

Once the routine runs, `ux-review-ui` and `ux-review-cli` stop running the runner themselves: each pass reads the latest `ux_rules_pipeline` run's filing output (counts, filed and commented tasks) and the open `ux-rules`-tagged tasks, treats every filed fingerprint as known, and keeps only judgement findings. With measurement gone they drop to `medium` complexity and a weekly cadence (`every_minutes` 10080). These are operator-configuration changes (this lane is not granted `orbit.auto_task.update`); apply them on the owner workspace as field-scoped patches alongside the phase-1/2 handoffs below: set `complexity` to `medium`, `every_minutes` to 10080, and replace each "Run the … rules first" section with "Read the latest ux-rules routine results first" wording that names `orbit run show` on the newest `ux_rules_pipeline` run and the open tasks tagged `ux-rules`.

## Handoff: ux-review-cli auto-task update

This change does not modify `ux-review-cli`. The definition is operator configuration, and this lane is not granted `orbit.auto_task.update`. The resumed implementation step must copy this handoff into the task execution summary. Recovery does not write that summary.

Apply it on the owner workspace with a field-scoped patch. Do not send a whole `template`: omitted template fields receive defaults. Do not change `enabled`, the schedule (`every_minutes` 4320), complexity (`hard`), tags, priority, or `required_tools`.

The patch drops the description sections "Help alignment and wrapping regression checks" and "Diagnostic summary spacing and cross-surface findings", and it drops the two acceptance criteria that re-measure grouped help and diagnostic summaries. The Lifecycle diagnostics card stays a dashboard finding for `ux-review-ui`.

```bash
orbit tool run orbit.auto_task.update --input-file ux-review-cli-template-patch.json
```

`ux-review-cli-template-patch.json`:

```json
{
  "name": "ux-review-cli",
  "workspace": "ws_orbit",
  "template_patch": {
    "description": "Review the user experience of Orbit's two programmatic surfaces: the `orbit` CLI, for operators, and the MCP server (`orbit mcp serve`, the `orbit.*` tools), for agents.\nCompare what you find with the previous pass, and file one Orbit task for each evidence-backed problem. This task files work; it does not change any command or tool itself. Its durable outputs are the review artifact and the filed tasks. A pass may legitimately finish with no new tasks, when nothing clears the bar below.\n\n## Safety\n\n- **Read-only against the live store.** Run read commands freely: `list`, `show`, `history`, `search`, `readiness`, `doctor`, `config show` and `--help`.\n- **Mutations only in a throwaway workspace.** To exercise a mutating command or tool (task add, update, approve, auto-task, run flows), use a fresh `orbit init` under `$ORBIT_SCRATCH_DIR` with `--root` pointing at a scratch Orbit root. Never touch the live store, a live drain or `orbit-web.service`.\n- **Mistakes on purpose, also in scratch.** Error paths (bad IDs, wrong flags, conflicting options, missing workspace, refused transitions) are part of the review. Trigger them only in the scratch setup.\n- **Disk gate:** before creating any worktree, build dir or scratch root, check `df --output=pcent <parent-dir> | tail -1`. At 80% or more, create nothing new and record the reading in the summary.\n\n## Run the CLI rules first\n\nBefore any judgement, run `node scripts/ux-rules/run.mjs --surface cli --orbit <binary under review> --out <scratch>` against a scratch root. Read `<scratch>/ux-rules.json`. Treat every `fail` as known: do not re-measure it and do not file a second task for it. A rule `error` is a broken rule, not a product finding. The rules already cover grouped-help alignment, hanging indent, help length, short help versus long help, JSON shape, error exit codes, MCP description length, and CLI/MCP required-input parity.\n\n## What to review\n\n1. **CLI.** Walk the commands operators use daily: `task` (add, list, show, update, settle-merged), `run` (auto, ship, show, history, readiness), `auto-task`, `workspace`, `doctor`, `config`, `browser-kit`, `web`, `agents` and `search`. Look at wording, dead ends, and whether help matches behaviour. Do not re-measure what a rule covers.\n2. **MCP.** Start `orbit mcp serve` in the scratch workspace and call the tools agents use most against the scratch store: `orbit.task.*`, `orbit.search`, `orbit.task.artifact.*`, `orbit.agents.*` and run inspection. Judge choice, result shape, and refusal text. Do not re-rank description length or re-check required-input parity; the rules did that.\n\nWhat counts as a finding:\n\n- **Broken:** a command or tool that fails on valid input, gives a wrong exit code, produces `--json` that isn't valid JSON or changes shape between calls, or makes a documented example fail, and that no rule already recorded.\n- **Dead end:** an error or refusal that doesn't say what to do next, or a state that no command can get out of, beyond the cases the error-exit rule already records.\n- **Clarity:** help text that is vague, contradicts behaviour, or exposes internal task or friction IDs; unexplained jargon; output without units or context.\n- **Verbosity:** help or descriptions that bury what the reader needs, beyond the length rules. The fix moves the detail into the docs (it is not deleted) and leaves a pointer.\n- **Consistency:** flags, nouns or verbs that differ for the same concept across commands; output formats that drift; a CLI/MCP disagreement the parity rule did not record.\n- **Discoverability:** a common workflow that needs an undocumented flag, or a feature reachable from only one surface without a reason.\n\nIgnore pure taste. Every finding needs a reproduction command, with the scratch `--root` if it mutates, the actual output, the expected behaviour, and a code pointer.\n\n## Compare with the previous pass\n\nFind the previous pass: `orbit tool run orbit.task.list` with `{\"status\":[\"done\",\"review\"],\"tag\":\"ux-review-cli\",\"limit\":5}`. Read its `ux-review.json` artifact. Mark each earlier finding as fixed, still present, or regressed. The first pass establishes the baseline.\n\n## Duplicate safety\n\nBefore filing:\n- Check the `surface-parity-sweep` auto-task's open work. It owns structural CLI, MCP and HTTP duplication, so file only the user-facing side of a parity issue.\n- List open work with `orbit.task.list` (statuses proposed, backlog, in-progress, review and blocked; tags `cli`, `mcp` and `ux-review`).\n- Search for the command or tool name with `orbit.search`.\n- If an open task already covers a finding, comment on it with the new evidence instead of filing a second one.\n\n## Filing\n\n- **Scope:** one task per root cause. A shared error helper that produces five bad messages is one task.\n- **Limit:** file at most 8 tasks per pass. Rank by impact (broken, then dead end, then clarity, then verbosity, then consistency, then discoverability) and list the rest in the artifact. One verbosity task may cover several commands that share a cause.\n- **Fields:**\n  - `type` is `bug` for broken findings and `feature` otherwise.\n  - Tags are `[\"ux-review\", \"cli\"]` or `[\"ux-review\", \"mcp\"]`, or both.\n  - Complexity is low or medium, or hard when the fix crosses crates or changes a persisted or external contract.\n  - `context_files` carry verified `file:` selectors.\n- **Description:** the reproduction command, the actual output, the expected behaviour, the user or agent impact, and code pointers.\n- **Acceptance criteria:** observable through the same command or tool call; never pin exact wording; note when a golden (CLI help or MCP text) must be regenerated.\n- Tasks land in `proposed`. Don't approve them.\n\n## Handoff\n\n- Store the full findings table as the artifact `ux-review.json` with `orbit tool run orbit.task.artifact.put`. Each row records surface, command or tool, category and severity; the reproduction; disposition (filed id, commented-onto id, or deferred); and status against the previous pass.\n- The execution summary covers the commands and tools reviewed, every filed or commented finding, the deferred findings, and anything you couldn't exercise and why.\n- The Lifecycle diagnostics card is a dashboard layout finding covered by `ux-review-ui`. Refer matching dashboard findings there. Do not infer a CLI defect from that screenshot.\n",
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

This change does not modify `ux-review-ui`. The definition is operator configuration, and this lane is not granted `orbit.auto_task.update`. Apply the generated patch — attached to the delivering task as `ux-review-ui-template-patch.json` — on the owner workspace with a field-scoped patch. Do not send a whole `template`: omitted template fields receive defaults. Do not change `enabled`, the schedule (`every_minutes` 4320), complexity (`hard`), tags, priority, or `required_tools`.

```bash
orbit tool run orbit.auto_task.update --input-file ux-review-ui-template-patch.json
```

The patch is `{"name": "ux-review-ui", "workspace": "ws_orbit", "template_patch": {"description": …, "acceptance_criteria": […]}}`, derived from the live definition at delivery time. It:

- adds a "Run the UI rules first" section: run `--surface ui` against the live dashboard (and the website manifest against the preview) before any judgement, read `ux-rules.json`, treat every `fail` as known, and use `--fixture` for the forecast-reason rule when live data has no unavailable forecast;
- replaces the visual-design measurement list (`getBoundingClientRect()` / `getComputedStyle()` sibling comparisons and `scrollHeight` checks) with the judgement remainder — misplaced affordances and wasted space — and routes a measurement-shaped judgement finding to a `ux-rules` task that adds the rule;
- deletes the three 2026-10-29 regression sections ("Viewport containment regression check", "Panel body padding regression check", "Backlog-forecast unavailable reasons and qualified baseline") and their three acceptance criteria, now covered by `ui.no-horizontal-overflow`, `ui.panel-body-padding` and `ui.forecast-unavailable-reason`;
- re-scopes the visual-design acceptance criterion to judgement findings read against `ux-rules.json`, and prepends a criterion requiring the runner-first pass.

To regenerate it after the definition drifts: read the current template with `orbit.auto_task.list`, apply the same four edits, and keep everything else byte-identical.
