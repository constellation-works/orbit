# Crew risk observational report

These scripts are for manual contributor analysis from an Orbit source checkout.
They are separate from a workspace's `crew-ladder-review` auto-task, where one
is defined, which uses workspace ladder and experiment readouts without invoking
these scripts.

`report.py` is the supported entrypoint. It exports facts read-only from an
explicit registered **owning** workspace, or consumes a frozen JSON/gzip snapshot,
then runs the original temporal backtest, same-tier comparison and size-response
plotting algorithms. It writes only a new explicit scratch output directory.
It never changes tasks, configuration, experiments, pools or effort, and never
installs dependencies. Replica exports refuse with a recorded gap rather than
pretending local telemetry is the owning corpus.

Use Python 3.12 with an explicitly prepared virtual environment. For example,
from a repository checkout (dependency installation is an operator setup step):

```sh
python3.12 -m venv .orbit/tmp/crew-risk-venv
.orbit/tmp/crew-risk-venv/bin/python -m pip install -r scripts/crew-risk/requirements.txt
.orbit/tmp/crew-risk-venv/bin/python scripts/crew-risk/report.py \
  --workspace <registered-owner-selector> \
  --output "$PWD/.orbit/tmp/crew-risk-<unique-run>" \
  --as-of 2026-10-30T06:31:09+00:00 --protocol observational-v2
```

The dependencies and transitive dependencies are pinned; the run manifest hashes
scripts and records protocol parameters and Python/numpy/matplotlib versions. No provider credentials are needed by the
numerical stages. The live export uses the selected workspace's installed Orbit
CLI and its existing read authority; `--orbit <candidate-binary>` selects a build
for disposable QA. It resolves `workspace show` and `config show` rather than
assuming a workspace slug or home directory. Read-only SQLite connections use
`mode=ro` transactions against the resolved task index and telemetry store. Task
listing is checked for completeness and retried at the reported total. Canonical
event files are read only. Git history comes from the registered integration
branch. Missing/unavailable sources produce a gap instead of a fabricated export.
The CLI may record ordinary read audit telemetry; this helper performs no
canonical task/config/experiment writes or run reconciliation.

For offline input, no Orbit command or store is accessed:

```sh
<python> scripts/crew-risk/report.py --input <snapshot.json.gz> \
  --output <new-scratch-directory> --protocol observational-v2 \
  --cutoff 2026-10-01T00:00:00Z --as-of 2026-10-30T00:00:00Z \
  --previous-manifest <downloaded-prior-manifest.json>
```

Outputs are `snapshot.json.gz`, `manifest.json`, `results.json`,
`within-tier-results.json`, `curve-fits.json`, supported
`risk-size-curves.{png,svg,pdf}`, and `report.md`. `--export-only` copies/exports
snapshot and manifest without scientific dependencies. `manifest.json` contains
input/output/script hashes, exact cutoff/as-of/protocol/seed, source coverage,
per-identity cohort counts/exclusions, previous snapshot hash and newly mature
IDs. Output directories must be new so stale figures cannot masquerade as fresh.
Missing dependencies exit 2 with a persisted manifest/report and an actionable
gap. No numerical results or plots are fabricated. Other source/input failures
also exit 2; a completed analysis with sparse/unknown/shadow-only evidence records
`complete_with_gaps`. When recording a manual analysis on a task, attach
its outputs as task artifacts, compressing/splitting files over the artifact
limit and preserving part hashes. They contain private task evidence and must
not be committed. The plotting font cache is confined to the output directory.

## Snapshot and support contract

Schema version 1 has `as_of` (timezone-qualified ISO timestamp), `git_head`,
`rows`, `links`, `limitations`, `history_read_failures`, and optional `coverage`.
Each completed row has `id`, `done`/`landed` (epoch seconds), `lines`, `model`
(exact execution model), `complexity`, `type`, size/model provenance `basis` /
`model_basis`, and optional `effort`, `configuration_digest`, `execution_mode`,
`mixed_models`. Effort may explicitly be `default` when a captured execution
contract proves it was unset; absent/null means unknown. A regression link has
`target`, `observed_proxy` (epoch seconds), `tags`, plus source/provenance fields.
Missing or false `coverage.task_enumeration_complete` or missing/failed source history makes
negative-label coverage unknown and suppresses fits. Count all observed rows and
exclusions; never treat unread source history as zero defects.

The original local telemetry records execution model but does not capture effort
or crew configuration digest. Live exports preserve those as **unknown**, never
infer them from today's crew configuration. `observational-v2` refuses to combine
such identities into curves/comparisons; its manifest retains observed counts.
An enriched snapshot with captured execution-time identity can supply them.
Ladder experiment readouts remain the authority for assigned/executed/settled,
14-day quality, grading adoption, total delivery cost/time and trial verdicts;
this helper's completed-task proxy endpoint cannot supply those absent facts.

The default protocol is `observational-v2`: all supported exact execution
identities are discovered dynamically, including future models, without a crew
alias table. Group keys encode `[model, effort, configuration_digest]`; all
comparisons/curves stay within recorded task complexity. Unknown tiers, mixed
models, shadow-only rows, incomplete labels and immature outcomes remain counted
but excluded. Default cutoff is epoch (full available history); choose an explicit
cutoff for a comparable policy epoch. No class pool is implicit. Optional `--pool`
JSON is `{"schema_version":1,"version":"my-pool-v1","members":["<exact group key>"]}`;
it retains per-member counts and never mixes tiers. Flash is not implicitly
pooled with Luna/Haiku/Spark. Pool comparisons are descriptive post-request screens.

Common-cell comparisons retain the original UTC calendar-week × task-type ×
`floor(log2(1+lines))` overlap weighting by `min(nA,nB)`, 20,000 conditional
permutations, seed 20094, Wilson intervals and Holm adjustment for the qualifying
individual-pair family. Temporal evaluation retains the fixed seven-day folds,
training outcomes matured before the test cutoff, minimum 100 training tasks /
10 events / 30 test tasks, shrunk cloglog model terms and 2,000 fold-cluster
bootstrap draws. Plots retain the original unpenalized cloglog fit and 800 task
bootstrap refits (seed 20095), with support restricted to the group's 5th–95th
size percentiles. Unlike the brittle original assertion, sparse/singular fits
are suppressed: at least 30 rows, five outcomes of each class and at least 400
finite bootstrap refits are required. Counts and gaps survive suppression.

Seven-day observed links are exploratory ascertainment proxies, **not** the
ladder's mature fourteen-day quality endpoint or a proposed thirty-day risk
calibration. Size is a post-treatment final diff; routing, reviewer, calendar,
related-task and unobserved coverage differences prevent causal capability or
effort claims. Missing links do not prove absence of defects. Lower observed
risk does not authorize routing/configuration changes. No paid experiment or
quality grading is run here.

## Original provenance and intentional changes

`source-manifest.json` preserves the original source/output hashes, seed/cutoff
receipt and key counts from the source task's `backtest-2026-10-29/` artifacts.
The private frozen input SHA-256 is
`d6445bfd33beea3e350597945be1fc95a69c58ee393ed5e5872ea93af7e7811d`;
that corpus is deliberately not in this repository. The receipt includes exact
original source task/artifact identities for authorized retrieval.

`export.py` replaces the original home/slug/task-prefix assumptions with
registered workspace/checkout resolution, complete reads and explicit output.
`analyze.py` retains the original temporal routines; `compare.py` is the original
`within_tier.py` with parameterized cutoff/pool; `plot.py` is `plot_curves.py`
with its numerical fitting routine retained and cache probing/fixed model panels
replaced by dynamic same-tier identities and safe sparse-fit suppression.
`report.py` owns explicit parameters, dependency failure, provenance and deltas.
There is no hidden current-crew-to-historical-model remapping.

To replay the original model-only descriptive cohort, use `--protocol
historical-v1 --input <frozen-input.json.gz>` with its original as-of. This freezes
the four original source-policy cutoffs and explicit three-model user pool;
it deliberately retains unknown effort/digest and cannot support effort claims.
Temporal and within-tier results should preserve the original counts and numeric
results; plot layout/group discovery/suppression and serialized output hashes
intentionally change. Policy commit dates are not verified deployment times.
Historical-v1 is a reproducibility route, not evidence for a ladder revision.
