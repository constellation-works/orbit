---
type: runbook
summary: Select an admission forecast cohort and separate idle unsampled minutes from sampling gaps without changing host policy.
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

   Omit `--json` for a table. The three selectors intersect; each is optional
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
     the sum of `unsampled` and `gap`. Waiting for the forecast horizon
     alone does not repair either.
   - `unsampled`: the target minute has ended, has no host row, and no
     delivery run was live during it. Host minutes are written only by
     delivery workers, so a forecast made while the drain is idle has
     nothing to pair with. That is not a sampling defect.
   - `gap`: the target minute has ended, has no host row, and a delivery
     run was live during it. A delivery run is a worker of a submitted
     delivery pipeline or a claimed leaf. It is live from `started_at`
     until `finished_at` (and still live while `finished_at` is absent).
     The drain's own run is not a delivery: it records forecasts while
     idle and does not sample the host. A run that never started does not
     count. A run that finishes exactly as the minute starts, or starts
     exactly as the minute ends, does not cover that minute.
   - `paired`: the target minute has ended and has a measured row. These
     alone contribute to error summaries; the report does not certify how
     completely that minute was sampled. A live delivery in a paired
     minute does not move it into `gap`.

   At each offset, paired + pending + missing equals the selected recorded
   point count, and missing = unsampled + gap. `unmeasured` remains
   pending + missing. Text columns `PAIRS`, `PENDING`, `MISSING` and the
   error columns keep their meaning; `UNSAMPLED` and `GAP` follow
   `MISSING`. JSON adds `unsampled` and `gap` beside `missing`. Not every forecast
   necessarily records every offset (the configured horizon can cap it).
   An empty cohort has zero counts, empty pairs/drains and null errors.
5. Retain the raw pairs and the separate configuration evidence for live
   post-deploy evaluation. Error stays forecast minus measured: negative
   means under-forecast. Compare like cohorts and wait for target minutes
   to mature before judging them. Resource profiles from `orbit run profiles`
   describe a separate per-delivery population; sparse deliveries and wide
   spread remain limits on confidence, regardless of host-forecast pair count.
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
