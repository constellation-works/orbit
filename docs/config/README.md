---
type: context
summary: Reference for Orbit's runtime configuration and environment variables.
last_validated: 2026-11-22
---

# Orbit Configuration

Reference for Orbit's `config.toml`: every fixed key, its default and what it does. `orbit config keys` prints the same keys with descriptions. `orbit init` writes a minimal template, [`crates/orbit-config/assets/default-config.toml`](../../crates/orbit-config/assets/default-config.toml), with the agent environment allowlist, the Codex sandbox mode, review-skip mappings for shipped auto-task provenance tags, and the `[workflow]` table its crew keys go into; this page is the reference. The website has a shorter overview under [Reference › Configuration](https://orbit-cli.com/reference/config/). To add an execution lane rather than configure a shipped one, see the [executor onboarding runbook](../runbooks/executor-onboarding.md).

## Configuration reference

- [Environment variables](environment.md)
- [Machine configuration](machine.md)
- [Workflow configuration](workflow.md)
- [Workflow tags](workflow-tags.md)
- [Workflow validation](workflow-validation.md)
- [Crews and crew selection](crews.md)
- [Crew routing for failure and recovery](crew-recovery.md)
- [Providers and executors](providers.md)
- [Sandbox configuration](sandbox.md)
- [Execution configuration](execution.md)
- [Review and completion](review.md)
- [Pull requests, commits, and branches](delivery.md)
- [Operational settings and limits](operations.md)
- [Experiments, workspace seeding, and plugins](extensions.md)
- [Validation and errors](validation.md)

## Where config lives

| Path | Scope | Created by |
|---|---|---|
| `<workspace>/.orbit/config.toml` | Workspace-local (per user, gitignored) | `orbit workspace init`, hand-authored, or `orbit config set --fresh` / `--seed-from-global` |
| `~/.orbit/config.toml` | Global | `orbit init` (only when absent, or under `--force`) |

Settings inherit per key: workspace overrides global, global fills omissions, built-in defaults fill the rest. Keep a workspace file to genuine overrides.

Routine and auto-task schedules are not config keys: they live in the definitions under `<workspace>/.orbit/routines/` and `<workspace>/.orbit/auto_tasks/`. Change a schedule with `orbit auto-task update`, `orbit.routine.control` `set_trigger`, or the **edit** link on the dashboard's Automation rows, which edits only the schedule or trigger; see [`automation-routines.md`](../../crates/orbit-core/assets/skills/orbit-setup/references/automation-routines.md#changing-a-routines-trigger).

`orbit workspace init` seeds missing review keys for the registered ship mode
(a new registration defaults to `pr`; `--force` retains its mode unless
`--ship-mode` changes it). A PR workspace gets `review.timing = "before_landing"`,
`review.minutes = 30`, and `workflow.purpose_crews.pre_landing_review` containing
the enabled members of `luna` and `haiku` in the effective host/workspace config.
With neither enabled, timing is explicitly `"off"` and the pool is empty; init
never substitutes a costlier reviewer. A local workspace gets timing `"off"`
because before-landing review needs the PR route, plus the same explicit
30-minute budget. Each seeded key has a short comment explaining the choice.
Existing workspace values (including legacy review switches) win, and repeated
init leaves them unchanged. Global `orbit init` seeds no review timing and
keeps the built-in `off` default for other workspaces. When init creates the
first workspace policy layer, it explicitly preserves the sandbox, approval
policy and environment allowlist previously in effect; it leaves an existing
workspace policy layer's security settings alone.

Both config files must be regular files. Runtime loads and workspace plugin-toggle polling refuse symlink leaves (including dangling links), directories, FIFOs and other non-regular leaves. Symlinked parent directories are supported and resolved before opening the file. A missing file or parent directory contributes no layer, so global settings and built-in defaults still apply.

- **Tables** layer down to individual settings; **scalars and arrays** replace the global value.
- **Named crews** layer by crew name and field: a workspace `[crews.sol]` holding only `model = "gpt-6-astra"` overrides that one field of the global `sol` crew.
- **Security keys** (`execution.codex.sandbox`, `execution.codex.approval_policy`, `execution.env.pass`) never inherit from global once a workspace file exists; an omitted one takes its built-in default. A workspace file holding only `[plugin_enablement]` does not count, so those keys keep inheriting.
- `[machine]` and each crew's [`env`](crews.md#per-crew-credentials-env) are global-only and `[plugin_enablement]` workspace-only; any of them in the wrong file is refused at load, naming the file. A `[plugin_enablement]` table in the global file is a typed `host_configuration` error naming the host and file (not the caller's `invalid_input`), so a follower whose remote call the owner serves is classified by final recovery as transient infrastructure and requeues once the table is removed. `orbit doctor` (and the dashboard's Doctor panel, which runs the same probes) reports it as the `plugin-enablement` check with the exact fix: remove the table from the global file; host enablement is `orbit plugin enable|disable <plugin>` and a per-workspace toggle adds `--scope workspace`. Because the table stops the runtime from opening, a plain `orbit doctor` reads it straight from the global config this invocation would load — `~/.orbit/config.toml` by default, or the config under the resolved `--root`/`ORBIT_ROOT` data directory (including an initialized `<checkout>/.orbit` child for `--root <checkout>`) — and reports that check (plus a `runtime` row saying no other check ran) instead of the bare refusal; every other command, and doctor's repair flags and subcommands, keep failing closed.

`.orbit/config.yaml` (it stores `workspace_id`) is the workspace identity file, not runtime config.

## Inspecting and editing

| Command | What it does |
|---|---|
| `orbit config show` | Effective merged view. Identity (`machine.id`, `machine.task_prefix`) leads, then Machine, Delivery (`workflow.*`), Crews, Execution, Review and Housekeeping. Each row's description is one sentence, followed by the key's allowed values in brackets when it has any; defaults and interactions are in this reference. Unconfigured advanced keys are hidden with a count per section. `--all` (alias `--advanced`) lists every key, expands unset sections, and appends Paths, Crew purposes and Review, which restate rows above. `--json` adds `provenance` and always includes `crew_purposes` and `review`. |
| `orbit config get <key>` | One value. |
| `orbit config set <key> <value>` | Write the workspace file (`--global` for the global one). The value parses as a TOML literal, else a string. Without a workspace file, pass `--fresh` (start empty) or `--seed-from-global` (copy the global file without its global-only keys: `[machine]`, each `[crews.<name>] env`, `lease.intake.peer_user` and `[lease.listener]`). |
| `orbit config keys` | Every fixed settable key with type, section, description and allowed values (a fifth, last column). `--json` adds `detail` (longer guidance, or `null`) and `allowed`. |
| `orbit config path` | The resolved `config.toml` path. |

`config set` checks a registered setting's supplied value type before staging the edit. A type mismatch names the key and supplied value, explains the expected type, and confirms the edit was not saved. For example, `orbit config set --global review.minutes abc` is refused; supply an integer such as `30` instead. Full config validation still runs before saving. Errors loading an already invalid config identify its file.

`config get` and `config set` refuse unknown keys. For a misspelled fixed key such as `workflow.base_branc`, text errors suggest a nearby key and point to `orbit config keys` for the valid-key list. JSON errors keep the `invalid_input` code and include up to three nearby keys in `did_you_mean`, closest first; when none is close enough, that field is omitted. Refused writes leave the configuration unchanged. Removed keys retain their migration guidance.

**Allowed values.** Every registry key has a description of at most 100 characters, and keys that accept more than their type allows also declare `allowed`. In `--json` (`config keys`, the `show` provenance, and the dashboard's config rows) it is `null` for a bool, an unbounded number or a free-form string, and otherwise one of:

- `{"kind": "enum", "values": [...]}`: exactly one of the values.
- `{"kind": "enum_list", "values": [...]}`: a list whose every entry is one of the values.
- `{"kind": "range", "integer": true|false, "min", "max", "unit"}`: a number in `min..=max`; `max` is `null` when only the minimum is bounded.
- `{"kind": "format", "note"}`: a free-form value shaped as the note says.

Each also carries `summary`, the one-line form the tables print (`off | login | interactive`, `1–10 rounds`, `≥ 1`). Config load and `config set` check the same declaration, so a value outside it is refused with `<key> has invalid value <v>; expected …` naming the declared range or choices. The Settings topic pages show each row's description and allowed values; editing an enum offers only its values, and a range field refuses a value outside its bounds before saving.

**Advanced** keys are shown whenever any layer sets them, including a global security value that is not inherited. JSON always includes every key. Scoped text views apply the same tier filter to the selected file. The tier affects display only: loading, `config get`, and `config set` are unchanged.

Each `show` value has one state: `workspace`, `global` or `environment` (the layer that set it), `default` or `unset`. `machine.id` and `machine.task_prefix` are not config keys in the text view: they render under Identity with state `identity`. JSON `state` stays `set`, `default` or `unset` for every key, including those two. A row names a shadowed layer: `(overrides global: main)`, or `(global sets danger-full-access — not inherited)` for a security key the workspace omits. A registered checkout also gets a `Workspace` line with the registry's base branch and ship mode, which delivery uses. In `--json`, each key's provenance has `scope`, `path`, `section`, `tier` (`standard`/`advanced`), `description`, `allowed`, `state` (`set`/`default`/`unset`) and `shadowed_by` (`[{layer, value, reason}]`, reason `overridden` or `not-inherited`); top-level `workspace_binding` is the registered base branch and ship mode, or `null`.

`--scope global|workspace` shows one file's values plus built-in defaults, without inheriting the other layer. Crew references are still validated against both layers' crews (partial workspace overrides included), here and in the dashboard's Workspace file view; a crew in neither is reported by name and key. A dashboard file validation error names the file and asks you to fix it before reloading. Scoped JSON reports `exists` (`config get`: key in that file) and `source.exists` (`config show`: file exists).

The dashboard's Settings payloads (`GET /api/config/effective`, `GET /api/config/file?scope=…`) add per-key comparison facts to each row. `built_in` is `{state, value}`: the value Orbit ships with, `state` `unset` with a `null` value when there is none. `fallback` is `{layer, value, state}`: what the key resolves to under the current layering if the layer that sets it stopped setting it, or `null` when nothing can be removed or no admitted fallback can be predicted. That is a prediction of the effective value only. The guarded removal behind **Remove all** in Settings › Other (`DELETE /api/config/keys/{key}?scope=workspace&preserve_effective=true`) makes a separate decision under the file's write lock, from the serialized document that would be saved. It refuses with `409 effective_value_would_change` when that document would no longer load as a workspace policy layer (a file left holding only `[plugin_enablement]`), or when the key's effective value there would differ from the current one, even if the current value equals `fallback.value`. A `null` fallback is refused as `unknown_fallback`. A matching fallback is therefore a candidate for removal, not a guarantee. Removing a workspace security key falls to the built-in value, not the global one, while a workspace file exists. `equals` is `{global, built_in}`: whether the row's value restates the global file's value (`null` when the global file does not set the key, and on the Global file view itself) and the built-in default, by plain JSON equality, so `opus` and `opus:100` differ. On the Global file view, `reach` says what a set value does for the bound workspace: `{state: "in-effect"}`, `{state: "overridden", workspace_value, equals_global}` or `{state: "not-inherited"}`. On the file views, `fallback`, `equals` and `reach` are `null` for rows the file does not set, and also when the two layers do not load together. Each row carries the registry's `description`, `detail` and `allowed` for its key. Every row also says what `orbit config set` would refuse: `read_only_reason` is the refusal note for a key no write may change (`null` otherwise), and `global_only` marks a key only the global file may carry, such as `machine.*`. The file payload also carries `text`: the file's raw TOML (empty when the file does not exist), shown by Settings › Other. File views do not build crew rows. They report `crews_enumerated: false` and list the file's `[crews.<name>]` tables in `crew_tables`, so an empty `crews` list does not mean no crew is defined.

For crew fields, the library’s `EffectiveConfigValue.fallback` describes removal of one field while its table survives. A workspace field inherits a lower global field when the resulting crew can be admitted. Otherwise optional fields use the configured entry’s defaults (`enabled = true`, empty `tags`, absent `description` and `effort`), even for a crew with a shipped name. Removing `model` or `provider` without a valid lower file value leaves the table invalid, so the prediction is `None`. The separate `built_in` value still describes the shipped crew of that name; shipped crew fields do not fill omissions in configured tables.

For registered settings, fallback values and global comparisons use the config layer's admitted JSON in the current workspace's settings and crew context. For example, a global `workflow.purpose_crews.review = "sol"` supplies `["sol"]`, and complexity pools are compared in canonical crew order with duplicate bare members collapsed. `shadowed_by.value` keeps the raw file projection, including a scalar crew or the written pool order. If a shadowed global setting cannot be admitted in that context, its comparison is `null`; its removal fallback is also `null` rather than predicting a successful reload. Replace-only security settings still fall back to built-in values.

---

## Resident evolution ledger

Resident checkpoints record capability gaps, with their occurrences and cost, in the
workspace's evolution ledger. `orbit evolution add|list|show` and the `orbit.evolution.*` tools
record and read ledger records; the operator-only `orbit evolution decline` suppresses one. Nothing files evolution proposal tasks automatically: the weekly
evolution pass routine was removed [ORB-22844]. `orbit resident digest --json` reports state,
escalations, vetoable decisions, the top five pains, open proposals and metrics; `--top` changes
the pain limit (1–100), and `--since` accepts a span or RFC 3339 window start.

The digest's default window and the ledger-cost span for a decline are fixed at 14 days. A
decline suppresses proposals for 30 days by default; `--days` overrides the duration for
that individual decline. A declined record reopens early only when its cost within the window
doubles. `resident.evolution.window_days` and `decline_suppress_days`, which set these defaults
before, are [deprecated](validation.md#validation-and-errors), as are the removed pass's thresholds
(`min_occurrences`, `min_distinct_cycles`, `min_distinct_mandates`, `min_cost_minutes`,
`max_proposals`).
