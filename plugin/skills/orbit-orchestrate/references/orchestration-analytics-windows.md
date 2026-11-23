# Choose count-based analytics windows

## Count-based analytics windows

Use a count-based window when you want a recent sample that stays populated
through quiet periods or uneven activity. It follows the newest records,
whatever their age. Use a time window when the question is about a specific
period, such as before and after a change.

For admission forecast accuracy, `orbit run profiles --accuracy --last 500`
selects the newest 500 matching forecasts, regardless of age. `--since`,
`--drain` and `--mode` narrow the cohort first, and `--last N` then keeps its
newest N forecasts; the selectors intersect. Without filters, the report uses
the newest 5000 forecasts. With `--since` and no `--last`, it includes every
matching forecast since that timestamp.

Check the reported selection and
report time before reading the errors; `--json` includes `selection`,
`report_at` and `since` so the effective cohort is explicit.

For per-task agent analysis, select a count window on the implementer
scorecard, for example:

```bash
orbit agents scorecard --role implementers --window last500 --per-task --json
```

`last50`, `last200` and `last500` count the newest delivery attempts in the
scan, whatever their age; `--per-task` exports one row per implemented task.
In the JSON header, verify `requested_window`, `window_attempts`, `since` and
`tasks_scanned` before comparing or exporting results. Use the scorecard's
calendar windows when you need a fixed elapsed period instead.
