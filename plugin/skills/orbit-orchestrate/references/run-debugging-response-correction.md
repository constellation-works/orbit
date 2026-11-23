# Inspect an agent response-correction round

## Bounded response-correction rounds

A read-only activity that declares a `response_correction` policy gets a bounded second chance when its agent reply is rejected for formatting alone. The parser stays the authority: the rejected reply never becomes a result. A fresh invocation with the same crew, tools and sandbox runs inside the unspent part of the activity's wall-clock budget, and only a reply that itself parses succeeds. This is not step recovery and never extends the budget.

Only read-only activities declare it, such as the task pilot, so a correction never repeats a mutation.

The fresh invocation receives one `rejection`:

- `contradictory_reply`: the reply said success and also carried an error object.
- `missing_envelope`: a clean turn ended in prose with no completion envelope, so nothing in it counts yet.
- `structured_output_retry_exhausted`: the provider exhausted its formatting retries; the request carries only the terminal diagnostic, not an answer.

A requested correction that cannot start records one `refusal`:

- `rounds_exhausted`: the step already spent its `max_rounds`.
- `wall_clock_exhausted`: less than the policy's minimum remaining budget was unspent.

### Read a correction without re-running

Do not re-run the activity to see whether a correction happened. Inspect the finished step:

```bash
orbit run show <run_id> --json
orbit run show <run_id> -s <step_id> --json
```

The step output carries `response_correction` with `rounds`, `max_rounds`, `refusal` (null when the last request ran), `original_error` and `original_stdout_blob_ref`. `rounds: 0` with a `refusal` means the first rejection was never corrected; `rounds >= 1` means at least one fresh invocation ran.

A still-failing step appends `after N response correction(s)` to its message, plus `(correction refused: <code>)` when a refusal stopped the loop and the original error after `original error before correction:`. The rejected reply's trace and stdout blob stay in the run's captured output, so compare `original_stdout_blob_ref` against the latest invocation before concluding what the agent said.
