# UX rules

Phase 1 runner for the CLI rule set. UI rules are a later phase and are not in this tree. The runner is a scheduled measurement, not a repository test and not a merge gate.

## Run

```sh
node scripts/ux-rules/run.mjs --surface cli --orbit /path/to/orbit --out /path/to/scratch
node scripts/ux-rules/run.mjs --surface cli --rule cli.help-alignment --orbit /path/to/orbit --out /path/to/scratch
node scripts/ux-rules/run.mjs --surface ui --out /path/to/scratch
```

`--surface` is `cli` or `ui`. `--out` is the scratch directory the run may create. `--orbit` is required for `cli` and is the orbit binary under test. `--rule` limits the run to one rule id (the full id or its last segment).

Each CLI run calls `orbit init` in a fresh directory under `--out` and passes `--root` at that scratch root. It sets `HOME` inside that directory and does not point at the live Orbit store.

`--surface ui` loads `scripts/ux-rules/ui/`. With no modules there, the runner writes no report and exits non-zero.

The runner exits 0 when it has written `ux-rules.json` for `cli`, including when individual results are `fail` or `error`. It exits non-zero when arguments are wrong, the binary is missing, no rules match, or the report cannot be written.

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
