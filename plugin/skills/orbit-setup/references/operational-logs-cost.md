# Operational logs: cost accounting and admission diagnostics

## Prompt-Size Cost Accounting

Prompt-size tiers are selected per model request, using that request's whole
input-side prompt: uncached input plus cache reads and cache-creation tokens,
including a separate longer-lived cache-write bucket when the provider reports
one. When per-request usage is available, every request that reaches a tier's
threshold is priced at that tier's rates for the whole request; requests below
the threshold use the base rates.

Invocation-wide totals cannot show which
individual requests reached the threshold.

Orbit derives an estimate from the recorded model, time and available usage
against its versioned model-rate table. A provider-reported amount, when
available, is separate from this derived estimate. Neither figure is a promise
about an external invoice or a claim about current provider prices.

Treat an unknown derived cost as unavailable, not as zero. It can mean that no
rate row covers the model and time, the reported input and cache buckets are
inconsistent, or a prompt-size tier might apply but per-request usage was not
captured. Without per-request usage, an invocation total below the tier
threshold proves no request could have reached it, so base-rate cost remains
known; a total at or above the threshold cannot identify the qualifying
requests, so cost stays unknown.

A captured empty list means all relevant
requests stayed below the capture floor; a missing list means that request
detail was unavailable.

To check a cost figure, open **Analytics › Cost**, choose the workspace and
time window, and review **Provider-reported** and **Derived estimate** with
their coverage counts. Use **Same-invocation comparison** only when its count
is nonzero; the separate totals can cover different invocations. The model
token totals show normalized usage attribution, not a cross-provider cost
ranking.

If derived coverage is short, use the model and time range to check
whether a rate applies, then whether request-level usage is available for
invocations that could have crossed a prompt threshold. A gap in coverage is
not evidence of zero usage or zero spend.

## Delivery Profiles and Admission Diagnostics

Inspect these read-only surfaces when work waits despite a free worker slot:

```bash
orbit run profiles --json
orbit run profiles --accuracy --json
orbit run readiness --json
orbit config keys --json
orbit config show
```

Profiles summarize per-delivery CPU and memory by workspace, work class and
complexity on this host; `auto-task:<template>` profiles omit complexity.
Each key has a union window across crew models and a separate window per
crew model, each using its last `profile_window` completed deliveries, whatever
their age. Orbit versions never filter these windows. Resumed or retried
deliveries are excluded.

Sparse keys fall back to broader profiles; inspect
sample counts and spread before drawing conclusions. `BUILD_FLAG` identifies
no-diff work using more than one core for two consecutive minutes, with its
deliveries listed; templates that build by design, such as QA sweeps, are exempt.
It is diagnostic evidence, not proof of a defect.

`--accuracy` compares forecasts recorded in `shadow` or `enforce` with measured
host load 5, 10, 20 and 30 minutes ahead, and reports shadow admission differences.
Error is forecast minus measured; negative errors are underestimates. Unmeasured
points are reported separately and cannot establish accuracy. Forecast modes
and settings are in [configuration-settings-workflow.md](configuration-settings-workflow.md#common-settings).

Forecast fit, the fixed worker-slot ceiling, and sustained resource throttling
are separate admission checks; diagnostics authorize no policy change.

Readiness names `host_shutdown_scheduled` with the scheduled time and mode.
New unattended admissions hold within the fixed 180-minute window; later schedules are still visible while admissions continue.
Running work is left alone by the hold. Report the schedule; preserve the hold and operator capacity policy rather than raising
concurrency, changing forecast thresholds, or cancelling a shutdown automatically.
