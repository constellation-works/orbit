# Read the resource forecast

## Resource forecast view

**Analytics › Forecast › Resource forecast**
(`#analytics/forecast/resources`) shows the `machine.auto_concurrency`
admission forecast that the CLI prints with `orbit run profiles`. It reads the
same profiles and accuracy report and builds no forecast of its own. Its
header cards show:

- the mode the drain admits with (off, shadow or enforce) and its source: the
  host's `machine.auto_concurrency.mode`, or a mode set explicitly when the
  drain started;
- the host's cores and memory;
- the admit thresholds as percentages and as absolute limits;
- the live baseline, which is the drain's newest recorded admission pass as
  the Tasks drain card draws it, with its peaks and admit or hold decisions.

It has three tabs, each with its own address:

- **Profiles** (`#analytics/forecast/resources`): one row per delivery
  profile, with its delivery count, horizon, peak CPU and memory and the
  minute each lands, per-delivery p50/p90 peaks, spread and build-like
  deliveries. Sparse profiles, which fall back to their base class, are
  badged, and a filter narrows the rows to change, no-diff, auto-task or
  sparse profiles. The per-minute curves are in **Analytics › Resources**.
- **Accuracy** (`#analytics/forecast/resources/accuracy`): forecast error per
  offset over the newest 5,000 host forecasts (`orbit run profiles
  --accuracy`). Two tables: the covered cohort first (the forecast against its
  saved baseline plus the deliveries it covered, the admission model's own
  error), then the whole host, which also counts deliveries admitted after the
  forecast.

  - Each shows paired minutes, mean and absolute CPU and memory error,
    and the share forecast under the measured load; only covered-cohort shares
    above 60% are marked. The whole host table also shows unmeasured minutes. The
    worst CPU and memory misses with their runs are listed for each population.
- **Decisions** (`#analytics/forecast/resources/decisions`): per drain run,
  newest first, its mode, passes and admissions, the tasks enforce held, and
  the tasks shadow would have held or added. When a drain sent pull requests,
  their sent and held counts are shown too. Hovering a count lists its task
  ids, and a run link opens that drain's recorded admission pass (`orbit run
  show <run-id>`).

The view is read-only. To change the mode or thresholds, use the
`machine.auto_concurrency.*` settings in
[configuration-settings-workflow.md](../../orbit-setup/references/configuration-settings-workflow.md#common-settings).
