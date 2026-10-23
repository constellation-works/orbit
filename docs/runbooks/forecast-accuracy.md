---
type: runbook
summary: Select a forecast cohort, compare raw and attributed errors, and distinguish idle, partial and missing measurements without changing host policy.
---

# Evaluate a forecast cohort

Use the shipped read-only accuracy report on the host whose forecasts and
measurements you need. It neither samples the host nor changes admission,
budgets, thresholds, profiles or persistent settings.

1. Choose the forecast start timestamp, drain run id and recorded mode from
   the operational change you are evaluating. Keep the deployment/source,
   build-budget and threshold evidence separately: forecast rows do not
   attest those values, and a time/drain selection cannot prove the
   configuration stayed homogeneous.
2. Capture the cohort in JSON, replacing the example timestamp and drain:

   ```bash
   orbit run profiles --accuracy --since 2026-10-15T00:00:00Z --drain jrun-example --mode enforce --json
   ```

   Omit `--json` for text. Text starts with a summary per offset: `PAIRS`,
   `CPU_MEAN_ERR (cores)`, `CPU_MAE (cores)`, `CPU_UNDER (%)`,
   `MEM_MEAN_ERR (GiB)` and `MEM_UNDER (%)`. Add `--wide` for every
   per-offset column the steps below name. Over ssh or a pipe, add
   `--header` to label the plain columns. The three selectors intersect; each is optional
   and requires `--accuracy`. `--since` is an inclusive RFC3339 forecast
   timestamp, with an explicit timezone. Drain and mode are exact matches;
   mode accepts `shadow` or `enforce`. Omitting since uses 30 days before the
   report timestamp. An unknown drain gives an empty cohort; malformed
   selectors or a start after report time fail. Without any selectors the
   legacy population is all recorded drains and modes in the last 30 days.
3. Check the effective selection before interpreting errors. Text names
   since, report time, drain and mode; JSON includes `report_at`, `selection`
   and the compatible top-level `since`. Null drain/mode means unrestricted.
   The same selection applies to forecast counts, pairs, errors and drain
   decision counts, including pull request counts. Only forecasts at or
   before the single report-time snapshot are considered.
4. Read maturity per offset (+5, +10, +20, +30 minutes):

   - The target minute starts at the forecast timestamp truncated to a UTC
     minute plus the offset. It ends one minute later.
   - `pending`: the target minute has not fully ended at report time. Even
     a partly collected measured row stays pending. A delivery that is live
     during that minute does not make it a gap yet: the sampler offers the
     host row only for a minute it observed whole.
   - `missing`: the target minute has ended but has no measured row. It is
     the sum of `unsampled`, `partial` and `gap`. Waiting for the forecast
     horizon alone does not repair any of them.
   - `unsampled`: the target minute has ended, has no host row, and no
     delivery run was live during it. Host minutes are written only by
     delivery workers, so a forecast made while the drain is idle has
     nothing to pair with. That is not a sampling defect.
   - `partial`: the target minute has ended, has no host row, and a delivery
     overlapped it, but no delivery was running for the whole minute. The
     sampler writes a host row only for a minute a worker saw whole: the
     worker was already running before the minute began (`started_at`
     strictly before it) and was still running when the minute ended
     (`finished_at` absent, or at or after the end). The minute a delivery
     starts in, and the minute it finishes in, are partial unless another
     worker covered them whole. That is not a sampling defect.
   - `gap`: the target minute has ended, has no host row, and a delivery
     was running for the whole minute, so the sampler should have written
     a row. A delivery run is a worker of a submitted delivery pipeline or
     a claimed leaf. It overlaps a minute from `started_at` until
     `finished_at` (and still overlaps while `finished_at` is absent).
     Overlap alone is not a gap. The drain's own run is not a delivery:
     it records forecasts while idle and does not sample the host. A run
     that never started does not count. A run that finishes exactly as
     the minute starts, or starts exactly as the minute ends, does not
     cover that minute.
   - `paired`: the target minute has ended and has a measured row. These
     alone contribute to error summaries; the report does not certify how
     completely that minute was sampled. A live delivery in a paired
     minute does not move it into `gap`.

   At each offset, paired + pending + missing equals the selected recorded
   point count, and missing = unsampled + partial + gap. `unmeasured` remains
   pending + missing. `--wide` text columns `PAIRS`, `PENDING`, `MISSING`
   and the error columns keep their meaning; `UNSAMPLED`, `PARTIAL` and `GAP` follow
   `MISSING`. JSON adds `unsampled`, `partial` and `gap` beside `missing`. Not every forecast
   necessarily records every offset (the configured horizon can cap it).
   An empty cohort has zero counts, empty pairs/drains and null errors.
5. Compare raw and attributed errors at each mature offset. Error stays
   forecast minus comparator: negative means under-forecast. Raw `cpu` and
   `memory` summaries compare against the measured whole host, including
   deliveries admitted after `forecast_at`. Attributed summaries compare
   against the baseline saved by the forecast plus only the measured load
   of deliveries it covered: running deliveries, outstanding reservations,
   and admissions from that pass (including returned pull claims). For
   example, a 5-core forecast with a 1-core baseline and 3 cores of covered
   load has attributed error +1; if later deliveries add 5 cores, the raw
   host error is -4. The latter alone cannot diagnose the forecast model.

   JSON `attributed_cpu` and `attributed_memory` each expose `mean_error`,
   `mean_absolute_error`, `under_rate` and `worst_miss`, beside the existing
   raw summaries. `--wide` text shows `ATTR_CPU_MEAN_ERR`, `ATTR_CPU_MAE`,
   `ATTR_CPU_UNDER`, `ATTR_CPU_WORST` and corresponding `ATTR_MEM_*`
   columns; raw MAE is `CPU_MAE` / `MEM_MAE`. `ATTR_PAIRS` (JSON
   `attributed_paired`) is the attributed denominator. Historical forecasts
   without the nullable baseline/cohort metadata still form raw pairs,
   but never contribute to attributed summaries; those summaries are null
   (text `-`) when no attributed pairs exist. Pair-level JSON adds nullable
   `attributed_cpu_cores` and `attributed_memory_bytes`, the comparator values.

   Run minutes are weighted by their sampled overlap with the UTC host
   minute and clipped at the next run minute and recorded run finish.
   Memory is an overlap-weighted sum of worker peaks, not a synchronized
   host peak. Known runs and continuations of their admission are covered;
   reservations resolve to their first measured admission, excluding later
   re-admissions of the same workspace/task. A covered delivery with no
   measurement adds no observed load. `ATTR_PAIRS` confirms recorded
   attribution metadata, not complete worker sampling. Inspect gaps or
   sparse worker measurements before tuning. The fixed baseline excludes
   later non-delivery host changes as well as later admissions.

   Retain raw pairs and separate configuration evidence for live post-deploy
   evaluation. Compare like cohorts and wait for target minutes to mature.
   Resource profiles from `orbit run profiles` describe a separate per-delivery
   population; sparse deliveries and wide spread remain limits on confidence,
   regardless of host-forecast pair count.
6. Read the per-drain decision columns on that same selection. Shadow
   columns count shadow passes only: `ADMITTED`, `WOULD_HOLD`, `WOULD_ADD`
   and `PULL_WOULD_HOLD`. Enforce columns count enforce passes only.
   `ENFORCE_ADMITTED` is the distinct tasks local enforce passes admitted.
   `GAP_HELD` is the distinct tasks gap fit held (`fit_skipped`); a task
   held on one pass and admitted on another counts in both. `PULL_HELD` is
   the enforce pull requests held because no work fit
   (`forecast_no_headroom`). JSON keeps the existing shadow fields and adds
   `enforce_admitted` (a count), `gap_held` (the task ids) and `pull_held`
   (a request count). `PULL_REQUESTS` and `PULL_SENT` still count both modes.

History is normally pruned after 30 days by run GC. An explicit earlier
timestamp can select surviving rows, but cannot recover pruned measurements
or infer historical source/settings. This reporting change does not establish
calibration or enforcement readiness. See
[accuracy design semantics](../design/auto-concurrency/2_design.md#81-the-accuracy-report).

## Reference: `orbit run profiles` columns

This is the column reference `orbit run profiles --help` used to carry.

Lists one resource profile per workspace, work class and complexity, learned from the CPU and memory each delivery's workers recorded. A delivery is the chain of runs for one task, counted from its admission; resumed or retried deliveries are left out. Each profile covers the key's last `machine.auto_concurrency.profile_window` completed deliveries from the last 30 days. `auto-task:<template>` profiles have no complexity.

HORIZON is the minute at which fewer than one in ten deliveries is still running, capped at `horizon_cap_minutes`. PEAK_CPU is the highest mean cores before it and PEAK_MEM the highest 90th-percentile memory, each with the minute it occurs. CPU_P50, CPU_P90, MEM_P50 and MEM_P90 are the nearest-rank 50th and 90th percentiles of each delivery's own highest minute. They stay high when deliveries peak in different minutes, so they can be several times PEAK_CPU; a sparse profile still shows them from however many deliveries it has. SPREAD says how far deliveries disagree with that average (per-minute standard deviation over mean, for CPU and memory): near 0 is a profile stable enough to forecast with, near or above 1 one that is not. DELIVERIES marks a key with fewer than `min_profile_deliveries` as sparse; a lookup for it falls back to a wider profile.

BUILD_FLAG marks no-diff profiles, and templates whose tasks are `no-diff-expected`, where a delivery used more than 1 core for 2 minutes in a row: work that should make no diff is building. The deliveries behind each flag are named below the table. Templates that build by design, such as qa-sweep, are never flagged.

`--json` emits every profile with its full per-minute series (alive, cpu_cores, cpu_stddev, memory_bytes, memory_mean_bytes, memory_stddev_bytes), its deliveries, its flagged deliveries, and `delivery_peak_cpu` and `delivery_peak_memory_bytes` (`p50` and `p90`).

`--accuracy` reports instead how well the host load forecast held up, from what drains in effective auto-concurrency mode `shadow` or `enforce` recorded each admission pass over the last 30 days by default: the forecast 5, 10, 20 and 30 minutes ahead, counting what the pass admitted, against the host's measured CPU and memory for that minute. Error is forecast minus measured: MEAN_ERR is its mean, MAE its mean absolute value, UNDER how often the forecast was below the measured load, and WORST the largest miss with the minute it forecast. By default each offset shows a summary: PAIRS, the CPU mean error, MAE and under-rate, and the memory mean error and under-rate, with units in the headers. `--wide` prints every column instead, including the worst misses, the unmeasured counts and the attributed (ATTR_) errors. PENDING counts target minutes that have not fully ended at report time, even if partly measured; MISSING counts ended minutes without measurements. MISSING splits into UNSAMPLED, when no delivery run was live in that minute, PARTIAL, when a delivery overlapped the minute but none was running for all of it, and GAP, when a delivery was running for the whole minute but wrote no host row. The sampler records a host minute only when a worker saw it whole, so the minute a delivery starts or finishes in is PARTIAL, not a sampling defect. PENDING plus MISSING is the compatible unmeasured count. Only ended measured minutes form error pairs. Below, per drain, its shadow passes against what it admitted: WOULD_HOLD counts the tasks it admitted that gap fit would have held, WOULD_ADD those gap fit would have admitted that it did not. ENFORCE_ADMITTED counts the distinct tasks local enforce passes admitted and GAP_HELD the distinct tasks gap fit held; both stay on enforce passes only. Pull drains record one forecast per request, counting a returned claim. PULL_REQUESTS counts these forecasts, PULL_SENT the requests sent to the owner, PULL_WOULD_HOLD the shadow requests enforce would have held because no work fit, and PULL_HELD the enforce requests held for the same reason. `--json` adds every forecast point paired with its measured minute, and names the gap-fit-held task ids.

Last, per drain, what limited its admission passes (`limiters` in `--json`), from the cumulative pass counters on its run state, which survive resume: PASSES counts them, and each outcome column is its share of those passes with the count. Every pass has exactly one outcome, the first that applies of ADMITTED (it started work), FAILED, HELD (an operator stop, a host shutdown, or a pull drain's other reason for sending no request), THROTTLE (host pressure), NO_CANDIDATE (an empty or wholly excluded backlog, or an idle owner), CEILING_FULL, GAP_FIT_HELD and DEFERRED (every candidate deferred on a lock or footprint), so the shares sum to 100%. DETAIL splits HELD by reason and THROTTLE by resource, and gives each waiting reason's share of passes, which overlap. The drains are those with a selected forecast and, without `--mode`, this workspace's drains live since `--since`; their counters cover each drain's whole lineage, not the selected time.

Accuracy selectors compose: `--since` is an inclusive RFC3339 forecast timestamp (default: 30 days ago), `--drain` matches an exact drain run id, and `--mode shadow|enforce` matches the recorded mode. Forecast timestamps are bounded by one report-time snapshot. Text and JSON echo the effective selection. This read-only report does not attest source revision, build budget or thresholds: a drain/time cohort is an operator selection, not proof of homogeneous configuration. Per-task profiles are a separate population; sparse deliveries or wide spread do not become calibrated confidence from selecting an accuracy cohort.
