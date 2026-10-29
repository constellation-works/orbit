---
title: "Scope: UX rules"
owner: claude
last_updated: 2026-10-29
last_validated: 2026-10-29
status: Draft
stage: designing
next: "Phase 1 (rule runner, result schema, CLI rules) is filed; build before the visual-review mock phases"
feature: ux-rules
doc_role: scope
type: design
summary: "Scope: break the ux-review-ui and ux-review-cli prompts into named, executable rules (Playwright measurements for the dashboard and website, PTY captures for the CLI and MCP surface), run them on a routine, and file one task per failing rule and target, so the LLM reviews keep only the judgement work"
tags: [ux-rules, ui, cli, mcp, routines, auto-tasks, browser-kit, visual-review]
---

# Scope: UX rules

Status: design only, nothing built. Daniel, 2026-10-29: "run a routine on periodic UI checks
on a list of rules ... break it down into rules, and automate the validation", and the same
for the CLI. This is the first slice of the shared visual infrastructure: the view manifest
and capture driver it introduces are the ones [visual review](../visual-review/1_scope.md)
reuses, and its UI rules become visual review's deterministic checks.

Bearing: two auto-tasks in `ws_orbit` review the user-facing surfaces by hand, each every
4,320 minutes (three days), at `hard` complexity, tagged `no-diff-expected`:

- **`ux-review-ui`** walks the dashboard and the website at 1440px and 375px in every theme
  with the browser kit and files at most eight tasks per pass. Its prompt lists findings to
  measure ("compare sibling elements' `getBoundingClientRect()` tops and heights and their
  `getComputedStyle()` padding, gap, font-size and font-weight") and, since 2026-10-29,
  three appended regression sections: viewport containment of the document and the active
  pane, the Lifecycle diagnostics panel's body padding, and the backlog-forecast unavailable
  reasons.
- **`ux-review-cli`** walks the `orbit` CLI and the MCP tools in a scratch workspace and files
  at most eight tasks per pass. Its prompt measures help length with `wc -c`, compares `-h`
  with `--help`, and, since 2026-10-29, checks grouped-help alignment at 60, 80 and 120
  columns through a PTY, and diagnostic-summary spacing.

Both store a findings table as `ux-review.json` and compare it with the previous pass. The
browser kit pins one Playwright build per Orbit release
([browser checks](../../DEVELOPMENT.md#browser-checks-on-a-prepared-host)). Jobs already
pair a collecting step with a deterministic filing step (`dependabot_alert_sweep_pipeline`:
`collect_dependabot_alerts` → `file_dependabot_alert_tasks`). [Script steps](../script-steps/1_scope.md)
(designed, not built) would let a job run a granted workspace script and read its JSON
stdout.

## Problem

Most of what the two reviews check is a measurement, and an agent re-takes it by hand every
three days.

- **The prompts accrete.** Each reported regression becomes another dated paragraph of
  prose. The `ux-review-ui` prompt now spends more words on three specific regressions than
  on its general method, and every word is paid for on every pass.
- **Coverage varies by pass.** An agent with a 40-view surface, eight filing slots and a
  context budget measures some views carefully and others not at all. A regression fixed
  last week can return unnoticed because this pass skipped the view. Comparing with the
  previous pass's table helps only for views both passes looked at.
- **Measurements are not reproducible.** Two passes measure the same panel with different
  scripts, widths and data, so "still present" and "fixed" are judgements, not results.
- **Judgement is crowded out.** The work only a model can do (unclear copy, wasted space, a
  confusing flow) competes for the same context as `scrollWidth` arithmetic.
- **Cost.** A `hard` agent run every three days per surface, mostly doing what a script does
  in seconds.

The fact that shapes the fix: the measurements in both prompts are already concrete enough
to write as code (each names the property, the comparison and the threshold), and both
surfaces have deterministic drivers on every kit host: Playwright for the UI, a PTY for the
CLI.

## Goal

The two reviews' measurable checks become named rules in the repository, each a small
module that returns pass or fail per target with its measurements. A runner executes every
rule against every target it declares and writes one results file. A routine runs it and
files one task per failing rule and target, deduplicated by a stable fingerprint, and
comments on that task when the rule passes again. The two auto-tasks then read the results
instead of re-measuring, and keep only the judgement findings. A newly reported regression
becomes a new rule, not a new prompt paragraph.

Unchanged: the delivery gate. Rules run on a schedule, outside the merge gate, so they add
nothing to it. They are not tests: the workspace adds tests only with bug fixes, and rules
are a separate, scheduled signal.

## Shape

### Rules

A rule is an ES module in the repository that exports its identity and a check:

```js
// scripts/ux-rules/ui/no-horizontal-overflow.mjs
export const rule = {
  id: "ui.no-horizontal-overflow",
  title: "No page or active-pane horizontal overflow",
  severity: "high",                       // shared scale: critical | high | medium | low | nit
  surface: "ui",                          // ui | cli
  targets: { views: "*", widths: [1440, 1166, 375] },
};

export async function check(ctx) {        // ctx.page: Playwright page at the target
  const m = await ctx.page.evaluate(() => {
    const d = document.documentElement;
    const pane = document.querySelector("[data-active-pane]") ?? d;
    return { doc: [d.scrollWidth, d.clientWidth], pane: [pane.scrollWidth, pane.clientWidth] };
  });
  const over = Math.max(m.doc[0] - m.doc[1], m.pane[0] - m.pane[1]);
  return over > 1
    ? ctx.fail({ measurements: m, excess_px: over, region: "viewport" })
    : ctx.pass({ measurements: m });
}
```

- **Surfaces.** `ui` rules receive a Playwright page already at the target view, width and
  theme. `cli` rules receive a runner that executes an argv in a scratch workspace and
  returns stdout, stderr, the exit code and, for PTY targets, the rendered screen at a
  verified width.
- **Targets.** UI targets are views from the view manifest
  ([visual review §3](../visual-review/2_design.md#3-the-view-manifest)) × widths × the
  view's themes and states. CLI targets are argv lists, or `"*"` for every command the
  binary lists, × widths for PTY rules.
- **Results.** `pass`, `fail` with measurements and evidence (a screenshot or a captured
  screen), `error` (the rule threw: a rule defect, never a product finding) or `skipped`
  with a reason.
- **Rules are reviewed like code.** They live in the repository, change through ordinary
  deliveries, and a rule that misfires is fixed or disabled in a diff with its reason.
- **Severity is the rule's.** A run never changes it; a rule whose severity is wrong is
  edited.

### First rule sets

Seeded from the two prompts. Each line is one rule.

| Surface | Rule | Measures |
|---|---|---|
| ui | `no-horizontal-overflow` | Document and active pane: `scrollWidth <= clientWidth + 1`; bounded table wrappers exempt. |
| ui | `panel-body-padding` | Every panel body has non-zero inset from its border and header divider, equal to its siblings'. |
| ui | `sibling-spacing` | Siblings of one kind in one container share padding, gap, font size and weight. |
| ui | `control-size` | Buttons in one toolbar or row share height, padding, font size and weight. |
| ui | `aligned-starts` | Neighbouring columns' first content starts at the same top. |
| ui | `even-row-panels` | Panels side by side in one row have equal heights. |
| ui | `bounded-length` | Page `scrollHeight` at most N viewports, and lists past N rows paginated or capped. |
| ui | `nested-card` | No bordered or filled box directly inside a bordered panel without a grouping role. |
| ui | `contrast` | Text at least 4.5:1 (3:1 for large text) against its effective background. |
| ui | `accessible-name` | Buttons, links, inputs and images have an accessible name. |
| ui | `console-clean` | No page errors, console errors or failed same-origin requests while rendering. |
| cli | `help-alignment` | In grouped help, sibling rows' descriptions start in one column with at least two spaces after the longest name, at 60, 80 and 120 columns through a PTY, and piped. |
| cli | `help-hanging-indent` | Wrapped descriptions keep one hanging indent and no hand-inserted line breaks. |
| cli | `short-help-shorter` | `-h` is shorter than `--help` wherever both exist. |
| cli | `help-length` | Per-flag help is one line; long help within a byte budget per command. |
| cli | `json-valid` | `--json` output parses and keeps its top-level shape across two calls. |
| cli | `error-exit-codes` | Common mistakes (unknown id, bad flag, conflicting flags) exit non-zero with a next step in the message. |
| cli | `mcp-description-length` | MCP tool and field descriptions within a byte budget. |
| cli | `mcp-cli-parity` | A CLI command and its MCP tool agree on required inputs and defaults. |

Rules that need the live data shape (the backlog-forecast unavailable reasons) are written
against API-shaped fixtures rendered in the browser, like the existing dashboard browser
tests, so they do not depend on what the live store happens to hold.

### The runner

The runner, proposed at **scripts/ux-rules/run.mjs**, runs with the kit's `node` and pinned Playwright:

```sh
. "${ORBIT_BROWSER_KIT_ENV:-$HOME/.local/chromium-deps/env.sh}"
node scripts/ux-rules/run.mjs --surface ui --base-url http://127.0.0.1:7878 \
  --out "$ORBIT_SCRATCH_DIR/ux-rules"
node scripts/ux-rules/run.mjs --surface cli --orbit "$(command -v orbit)" \
  --out "$ORBIT_SCRATCH_DIR/ux-rules"
```

- **UI against live, read-only.** By default UI rules run against the live dashboard
  (`--base-url`), as `ux-review-ui` does today, because real data is what exposes long
  tables and overflow. A rule never presses a control that POSTs: the runner blocks every
  non-GET request in the browser context and records it as an `error` of the rule that
  sent it. `--fixture` uses the manifest's fixture server instead, for rules that flap
  on live data.
- **Website** targets run against `npm run preview` in `website/`, light and dark.
- **CLI in scratch.** CLI rules run against a fresh `orbit init` under the output
  directory, with `--root` pointing at a scratch Orbit root, never the live store.
- **Determinism.** UI contexts use the capture driver's settings
  ([visual review §4.2](../visual-review/2_design.md#42-determinism)): fixed viewport and
  device scale, `reducedMotion`, animations off, UTC and `en-US`. PTY rules set the width
  through the PTY and record the width the renderer reported, not `COLUMNS` alone.
- **Output.** `ux-rules.json` plus evidence files:

```json
{
  "schema_version": 1,
  "surface": "ui",
  "commit": "…", "host": "hm_…",
  "kit": {"playwright": "1.x.y", "browser_revision": "…"},
  "target": {"base_url": "http://127.0.0.1:7878", "mode": "live"},
  "results": [{
    "rule": "ui.panel-body-padding",
    "target": {"view": "audit-diagnostics", "width": 375, "theme": "dark", "state": "default"},
    "status": "fail", "severity": "medium",
    "measurements": {"selector": ".lifecycle-diagnostics .tile-body", "padding_left_px": 0,
                     "sibling_padding_left_px": 16},
    "evidence": ["evidence/ui.panel-body-padding/audit-diagnostics-dark-375.png"],
    "fingerprint": "ui.panel-body-padding|audit-diagnostics|dark|375|default|.lifecycle-diagnostics .tile-body"
  }],
  "summary": {"pass": 412, "fail": 3, "error": 0, "skipped": 6}
}
```

The fingerprint is the rule id, the target and the failing element's selector (or the argv
for CLI), never a measurement, so the same defect keeps one fingerprint while its numbers
change.

### Filing

A generic deterministic activity, `file_rule_findings`, reads a results file and keeps one
task per fingerprint. It knows nothing about UI or CLI: any workspace script that writes the
schema above can use it.

| Result | Open task with this fingerprint | Effect |
|---|---|---|
| `fail` | none | File a `bug` task: title from the rule, description with the target, measurements and how to reproduce (the runner command with `--rule` and the target), evidence attached as artifacts, tags `ux-rules`, `ux-rule:<id>` and the rule's surface tags, `context_files` from the rule's declared source paths, status `proposed`. |
| `fail` | exists | Comment with the new measurements when they changed; otherwise nothing. |
| `pass` | exists | Comment once: the rule passes at this commit, with the measurements. No status change: the operator or the task's own delivery closes it. |
| `error` | — | One task per rule, not per target, tagged `ux-rules-error`: the rule is broken. |

- **Cap.** At most `ux_rules.max_new_tasks` (default 10) new tasks per run, highest severity
  first; the rest are listed in the run's results and filed on later runs.
- **Fingerprint storage.** The fingerprint is a task field written at filing
  (`rule_fingerprint`), so a lookup is an indexed query and a retitled task keeps it.
- **Severity floor.** Results below `ux_rules.file_at` (default `low`) are recorded only.

### The routine

```yaml
# .orbit/routines/ux-rules.yaml
trigger: {cron: "0 6 * * *"}
job: ux_rules_pipeline
```

`ux_rules_pipeline` has two steps: a [script step](../script-steps/1_scope.md) that runs the
runner for each configured surface, and `file_rule_findings` on its output. It needs script
steps' Linux execution phase. Until then, phase 1 below delivers the rules to the two
auto-tasks, which run the runner themselves.

### What the auto-tasks keep

Once the rules exist, `ux-review-ui` and `ux-review-cli` change:

- **Read first.** Run the runner (phase 1) or read the latest routine results (phase 3), and
  treat every `fail` as known: no re-measuring, no re-filing.
- **Judgement only.** Clarity, jargon, empty-state copy, wasted space, confusing flows,
  discoverability, docs accuracy against `--help`: findings no rule can decide.
- **Propose rules.** A judgement finding that is really a measurement is filed as a
  `ux-rules` task to add the rule, not as a UI fix only.
- **Prompt appendices move.** The 2026-10-29 sections become rules
  (`no-horizontal-overflow`, `panel-body-padding`, `help-alignment`, the forecast fixture
  rule) and are deleted from the prompts in the same change.
- **Cadence and size.** With measurement gone, both can drop to `medium` and run weekly; the
  routine runs daily.

### Configuration

| Key | Default | Meaning |
|---|---|---|
| `ux_rules.surfaces` | `[]` | Surfaces the routine runs (`ui`, `cli`). Empty disables it. |
| `ux_rules.runner` | `"scripts/ux-rules/run.mjs"` | The runner, as a granted script. |
| `ux_rules.ui_base_url` | `"http://127.0.0.1:7878"` | Live dashboard URL for UI rules. |
| `ux_rules.max_new_tasks` | `10` | New tasks per run. |
| `ux_rules.file_at` | `"low"` | Lowest severity filed. |
| `ux_rules.disabled` | `[]` | Rule ids not run, each with a reason in the config comment. |

### Surfaces

- **`ux-rules.json`** attached to the routine's run, and the evidence files.
- **`orbit run show`** for the routine's run: pass, fail, error and skipped counts, and the
  tasks filed or commented.
- **Filed tasks** carry `ux-rule:<id>`, so `orbit search --tag ux-rule:ui.contrast` lists one
  rule's open findings.

## Non-goals

- **A rule DSL.** Rules are code. A declarative format would need its own engine and would
  still need an escape hatch for every second rule.
- **Rules in the delivery gate.** They run on a schedule. Visual review's delivery-time
  checks reuse the UI rules on a candidate ([visual review §7](../visual-review/2_design.md#7-deterministic-checks)),
  in shadow first, but the gate itself gets no new required check.
- **Rules in Orbit core.** Rules and the runner are workspace files. Core gets only the
  generic results schema and `file_rule_findings`.
- **Replacing the judgement reviews.** The auto-tasks stay; they get smaller.
- **Auto-closing tasks.** A passing rule comments; it never transitions a task.

## Concerns and honest limitations

- **Live data moves.** A rule that passes on today's store can fail tomorrow because a task
  title got longer. That is often a real defect, but it also makes results vary run to run.
  Fingerprints keep one task per defect; a rule that flaps on data moves to `--fixture`.
- **Rules can be wrong about intent.** A deliberately dense toolbar fails `control-size`.
  The rule carries an allowlist of selectors with a reason, reviewed like code.
- **Selectors couple rules to markup.** A refactor that renames classes can turn rules into
  `skipped` (the target selector matches nothing). The runner reports a rule that matched no
  element on any target as `error`, so silent coverage loss is visible.
- **Script steps are not built.** The routine waits for their Linux execution phase. Phase 1
  gets most of the value without it.
- **Loopback access.** UI rules reach the live dashboard on loopback. Script steps' Linux
  phase runs without network; whether loopback counts is an open question for that design.

## Phasing

1. **Runner, schema and CLI rules.** The runner (**scripts/ux-rules/run.mjs**), the results schema, the PTY
   runner and the CLI rule set. `ux-review-cli` runs it first and drops its measurement
   sections.
2. **UI rules and the view manifest.** The view manifest format and Orbit's dashboard and
   website manifests ([visual review §3](../visual-review/2_design.md#3-the-view-manifest)),
   the capture driver's browser-context settings, and the UI rule set, including the
   forecast fixture rule. `ux-review-ui` runs it first and drops its measurement and
   regression sections.
3. **Routine and filing.** `file_rule_findings`, the `rule_fingerprint` field, the
   `ux_rules_pipeline` job and the routine. Needs script steps' Linux execution phase.
4. **Later.** Each filed only when needed: rules for other workspaces' UIs, fixture mode for
   flapping rules, and a dashboard view of rule results over time.

## Open questions

1. **Loopback from a script step.** Does the script-step Linux profile allow loopback when
   it denies network, or does the UI surface wait for the network phase?
2. **Where CLI rules live.** `scripts/ux-rules/cli/` beside the UI rules, or with the CLI's
   goldens in `crates/orbit-cli/`? Beside the UI rules keeps one runner.
3. **Close on pass?** A rule that passes on two consecutive runs could move its task to
   `review` for the operator. This scope only comments.

## Acceptance (feature level)

- `node scripts/ux-rules/run.mjs --surface cli` against a scratch workspace writes a
  schema-valid `ux-rules.json` with a result per rule and target, and never touches the live
  store.
- `--surface ui` against the live dashboard writes results for every manifest view at every
  rule's widths, issues no non-GET request, and records a rule that matched no element as
  `error`.
- The 2026-10-29 regressions are each caught by a rule: the Implementers panel overflow, the
  Lifecycle diagnostics padding, the grouped-help misalignment and the forecast unavailable
  reason.
- Two runs on an unchanged commit and store produce the same fingerprints.
- `file_rule_findings` files one task per new failing fingerprint up to the cap, comments
  instead of filing for an open one, and comments once when it passes.
- `ux-review-ui` and `ux-review-cli` prompts no longer measure what a rule measures, and
  their pass reads the rule results.

## Task References

- [ORB-19681] — the latest `ux-review-cli` pass
- [ORB-19682] — the latest `ux-review-ui` pass
- [ORB-19294] — designed script steps, which the routine needs

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
