# Configuration: mid-run steering

## Mid-run steering

Mid-run steering lets an operator's task comments guide a run at a later prompt
and prevents delivery from committing while an operator comment remains
undelivered. It works within the task's admitted scope and authority; enabling
it does not widen either. A new global config seeded by `orbit init` sets `workflow.mid_run_steering.enabled = true`; when the key is absent it resolves to `false`.

| Key | Default | Effect |
|---|---|---|
| `workflow.mid_run_steering.enabled` | `false` | Record steering watermarks and deliver later operator comments at task-context prompts; gate delivery while comments remain undelivered. |
| `workflow.mid_run_steering.max_rounds` | `2` | **Advanced.** Maximum steering rounds before a delivery with unconsumed comments is held. Accepts integers from `1` through `10`. |

For one workspace, set the values from its registered checkout with
`orbit config set` (which writes workspace configuration by default):

```bash
orbit config set workflow.mid_run_steering.enabled true
orbit config set workflow.mid_run_steering.max_rounds 2
```

The equivalent workspace `.orbit/config.toml` is:

```toml
[workflow.mid_run_steering]
enabled = true
max_rounds = 2
```

Read back both values and their configuration source, then check the doctor
row:

```bash
orbit config get workflow.mid_run_steering.enabled
orbit config get workflow.mid_run_steering.max_rounds
orbit config show --all
orbit doctor --check mid-run-steering
```

The doctor check reports `workflow.mid_run_steering.enabled` and
`max_rounds`; its row is `ok` whether steering is enabled or not, so inspect
the reported values. Settings refresh at prompt and gate checkpoints. Prefer
enabling between drains: when enabled during an existing run, missing
watermarks stay absent and comments since dispatch can be outstanding at the
next prompt or gate unless an inherited candidate or claim supplies a baseline.

Disabling takes effect at the next checkpoint and makes recorded watermarks
and round state inert without deleting them. While on, if comments remain
undelivered when steering rounds are exhausted or a round delivers none of
them, delivery stops before commit in the `steering_unconsumed` hold. See the
[mid-run steering operator procedure](../../orbit-orchestrate/references/mid-run-steering.md)
for posting and verifying steering comments.
