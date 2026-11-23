# Configuration: experiments

## Contents

- [Experiments](#experiments)

## Experiments

Experiments compare a control arm with one or more treatments for agent work.
Each treatment can vary instructions, skills, tools, crew, an allowlisted
configuration value, or agent memory (a memory arm). The `[experiments]` table
is workspace policy:

| Key | Default | Bound or effect |
|---|---|---|
| `experiments.enabled` | `false` | Boolean switch that permits experiments to start. |
| `experiments.config_keys` | `[]` | Advanced list of config keys an experiment may override. Empty allows no config overlays. |

For workspace policy, add or merge this table in
`<workspace>/.orbit/config.toml`:

```toml
[experiments]
enabled = true
```

At most 3 experiments run at once in a workspace, and a readout withholds its
verdict when any treatment arm's delivered rate is below 0.9.
`experiments.max_running` and `experiments.min_delivered_rate` load with a
deprecation warning, are ignored, and stop being recognised after 2026-12-08.

Each overlay config key needs two permissions: the built-in typed allowlist
and `experiments.config_keys`. The workspace list alone enables nothing else;
only allowlisted keys are supported, and any other key, a value of the wrong
type, or a number outside its bounds is refused when the experiment starts.
The built-in allowlist holds exactly two keys, and neither has an effect
today; both are kept so historical experiments still load:

- `workflow.explore.max_calls_per_run`, an integer from `1` to `100`,
  ignored since explorer removal.
- `visual_review.reviewer`, a boolean, ignored since visual review was
  removed.

To opt in, list only the keys the arms will override.
Read the workspace permission back with
`orbit config get experiments.config_keys`, and use
`orbit config show --all` to inspect effective values with their source, or
`orbit config get <key>` for one value.

An operator manages experiment definitions and lifecycle with these commands.
The syntax shows each command's required arguments; use `--help` for optional
flags and their values. Replace `<ID>` with the experiment's ID or name.

| Command | Use it when | Verify with |
|---|---|---|
| `orbit experiment create [OPTIONS] <NAME>` | Define a new draft. Options include `--hypothesis`, `--arms`, `--population`, `--primary-metric`, `--guardrails`, `--min-sample`, `--max-days` and `--mode`. | `orbit experiment show <ID>` |
| `orbit experiment update [OPTIONS] <ID>` | Change a draft's definition before starting it. | `orbit experiment show <ID>` |
| `orbit experiment clone [OPTIONS] --name <NAME> <ID>` | Copy an existing definition into a separate draft with a new name. | `orbit experiment show <NAME>` |
| `orbit experiment start [OPTIONS] <ID>` | Start a ready draft, opening its first segment and freezing its definition. | `orbit experiment show <ID>` |
| `orbit experiment pause [OPTIONS] --reason <REASON> <ID>` | Pause a running experiment when its enrollment should stop temporarily. | `orbit experiment show <ID>` |
| `orbit experiment resume [OPTIONS] <ID>` | Resume a paused experiment. | `orbit experiment show <ID>` |
| `orbit experiment reweight [OPTIONS] --reason <REASON> <ID>` | Change arm weights or retire arms in a running or paused experiment; this opens a new segment. Supply `--weights <WEIGHTS>` and/or repeat `--retire <RETIRE>` as needed. | `orbit experiment show <ID>` |
| `orbit experiment assign [OPTIONS] --task <TASK_ID> --arm <ARM> <ID>` | Force a nonterminal task into an active arm of a running experiment in shadow or live mode, when it has no assignment for that experiment or only an `explicit_crew` exclusion. | `orbit experiment show <ID>` |
| `orbit experiment conclude [OPTIONS] --reason <REASON> <ID>` | Finish an experiment and record the chosen arm with `--arm <ARM>`, or omit `--arm` to record no choice. | `orbit experiment readout <ID>` |
| `orbit experiment abandon [OPTIONS] --reason <REASON> <ID>` | End an experiment without recording a conclusion. | `orbit experiment show <ID>` |
| `orbit experiment delete [OPTIONS] --reason <REASON> <ID>` | Remove a finished experiment that never applied a treatment. | `orbit experiment list` (confirm it is absent) |

Existing arm assignments and exclusions remain sticky. Enrollment remains
historical, but concluded and abandoned experiments apply no treatment to a
fresh admission. Existing owner runs and claim receipts keep their frozen
treatment on resume. For a fresh crew admission, an explicit run choice or a
later audited task pin wins; a force supersedes an older task pin (and wins an
exact timestamp tie).

A crew override suppresses the whole crew-bearing arm and
records the intended digest and actual crew without rewriting enrollment. An
agent session may only use these read-only commands:

- `orbit experiment list`
- `orbit experiment show <id-or-name>`
- `orbit experiment readout <id-or-name>`

The other experiment operations are operator-only.

Choose `shadow` to record assignments without applying treatments. Choose
`live` to apply the assigned arm's instructions, skills, tools, crew and
allowlisted config values to work. A memory overlay is
`{"memory":{"roles":["implement"],"global":false}}`. `roles` lists
`implement`, `review` or `recovery` and defaults to `implement`. `global`
defaults to false and includes the shared global space when true. Memory is
its own surface, so one experiment per task may own it.

Memory has no switch
or command of its own: it is stored and delivered only while a running
experiment has a memory arm, and pausing or concluding that experiment stops
it. In `shadow` mode the owner records the memory capture and delivers
nothing. In `live` mode the assigned arm delivers it.

Verify state from the command line with
`orbit experiment list [--status <status>]`,
`orbit experiment show <id-or-name>` (arms and event log) and
`orbit experiment readout <id-or-name> [--metric <name>]` (per-arm outcomes
and intervals reported throughout; verdict reads `collecting` until the
horizon and `withheld` when a sample-ratio or treatment-delivery check fails).
Readouts calculate per-arm outcomes from the recorded assignments
and work records.

Or open the dashboard's **Analytics › Experiments** page
for the list with per-arm enrollment and each experiment's readout: the
detail renders from the experiment record immediately while the readout
loads beside it, so a slow or failed readout holds only the **Result** and
**Trust** tabs, which show their own loading or error state with a Retry
instead of failing the page. The header and **Setup** tab stay available.
