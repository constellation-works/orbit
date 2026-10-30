---
title: Visual Review — Design
owner: claude
last_updated: 2026-10-29
last_validated: 2026-10-29
status: Draft
feature: visual-review
doc_role: design
type: design
summary: Proposed design for mock-driven visual review — verify-mock-ui tasks carry mocks named by view, state, theme and width; the implementer compares its working tree with each mock through orbit visual capture; Orbit captures the mocked views on the final candidate, runs the workspace's UI rules, and a vision crew compares capture with mock under a precedence rule, with in-line verification, configurable blocking and an experiment gating the default.
tags: [visual-review, ui, mocks, review-gate, task-artifacts, finding-verification, experiments, browser-kit, ux-rules]
paths: ["crates/orbit-core/assets/tag_instructions/ui/**", "crates/orbit-core/assets/tag_instructions/verify-mock-ui/**", "crates/orbit-core/assets/jobs/task_pr_pipeline.yaml", "crates/orbit-core/assets/executors/**", "crates/orbit-web/visual/**", "scripts/ux-rules/**"]
related_features: [ux-rules, review-gate, task-artifacts, finding-verification, experiments, agent-scorecards, distributed-drain, user-interface]
related_artifacts: [ORB-19397, ORB-19304, ORB-11365, ORB-18351, ORB-13989, ORB-14849]
---

# Visual Review — Design

This doc specifies the mechanisms of [1_scope.md](./1_scope.md): where visual review sits in
delivery, mocks and how they name views, the view manifest, capture, the implementer's
self-check, hosts and claims, the deterministic checks, the comparison review, image
delivery per provider, budgets, findings and their verification, measurement, the later
before/after review, and the tag instructions. Nothing here is built. Where this doc names a
mechanism that exists today, it links its design; everything else is a proposal, and the
configuration keys are those in [1_scope.md › Configuration](./1_scope.md#configuration).

The view manifest (§3) and the browser-context settings (§4.2) are delivered first by
[UX rules](../ux-rules/1_scope.md) phases 1 and 2, whose UI rules are also this design's
deterministic checks (§7).

## 1. Placement in delivery

Visual review is one phase of the delivery job for a `verify-mock-ui` task, run on the
committed final candidate:

```text
implement (self-check available, §5) → commit → sync_base → validate
  → visual_capture      (deterministic, browser kit: candidate, one capture per mock)
  → visual_checks       (deterministic: the workspace's ui rules on those captures)
  → visual_review       (agent, vision crew, read-only)          ┐ skipped when
  → visual_verify       (agent, a different vision crew)         ┘ reviewer = false
  → visual_settle       (deterministic: record, file, hand to review)
  → review_gate_admit → review (code reviewer) → review_gate_settle → review_validate
  → visual_recapture    (deterministic, only when the review commit changed the candidate)
  → push → pr_open → …
```

Under `review.timing = "before_landing"` the same phase runs on the leaf or owner that runs
the landing review, before `landing_review_gate_admit`, on the head that review will settle.

**Why a separate phase that feeds the code reviewer:**

- **Not folded into the code reviewer.** The code reviewer is drawn from
  `workflow.purpose_crews.pre_landing_review` for code. Making it look at mocks either
  forces that pool to be vision-capable or silently reviews images with a lane that cannot see
  them. A separate step draws its own crew (§9.4) and can be measured on its own (§12).
- **Not a second review with its own verdict.** The review gate has one review per candidate
  and one reviewer fix commit ([review gate](../review-gate/1_overview.md), [ORB-13989]).
  `visual_settle` hands the code reviewer the confirmed findings, and the code reviewer
  decides and fixes, as it does for its own findings.

**How a blocking finding is resolved.** The code reviewer's prompt carries every confirmed
finding at or above `visual_review.block_at` as a *must address* item. Its certificate
records each one as `fixed` (its fix commit changes the cause) or `disputed` (with a reason).
Then:

1. When the review commit changes the candidate, `visual_recapture` captures every mock's
   view at the review commit and reruns the checks only. There is no second model round: one
   review per candidate still holds.
2. A blocking *check* finding still present after the recapture leaves the review
   `incomplete`, with the check's measurement as the evidence gap.
3. A blocking *comparison* finding marked `fixed` is shown with its mock, candidate and
   recaptured images in `visual/review.json`. A `disputed` one holds the candidate in review
   for the operator, who either accepts the dispute or rejects the candidate. Two crews (the
   reviewer and the verifier) confirmed it, so one crew's disagreement does not clear it.

**Without a pre-landing review layer.** With `review.timing` unset nothing can resolve a
finding before landing. Confirmed blocking findings are then filed as `high` follow-ups
(§11), the PR body lists them, and config load warns that `visual_review.mode = "live"`
without a review layer blocks nothing.

**When the phase runs.** Only for tasks tagged `verify-mock-ui` with `visual_review.mode`
not `off`. A task without the tag is untouched: no capture, no checks, no review.

## 2. Mocks

### 2.1 The tag

`verify-mock-ui` marks a task whose mocks are its visual specification. It implies the `ui`
tag's admission routing (kit hosts only, §6) and adds:

- tag instructions for implement and review (§14);
- mock validation at admission (§2.3);
- the visual review phase (§1);
- a draw of the implementer from image-capable crews (§5.3).

The task pilot warns (`utility_warnings: mocks_without_tag`) when a task has `mocks/`
artifacts but not the tag, and the operator decides; the pilot does not add it.

### 2.2 Names

A mock is an image task artifact under `mocks/`. Its name says what it shows:

```text
mocks/<view>[.<state>][.<theme>][.<width>].<ext>

  <view>   a view id from the view manifest (§3): lowercase, hyphenated
  <state>  default | empty | loading | error             (omitted: default)
  <theme>  a theme the view's app declares, e.g. dark    (omitted: the app's first theme)
  <width>  a positive integer, CSS pixels                (omitted: visual_review.default_width)
  <ext>    png | jpg | jpeg | webp
```

Examples, for the dashboard (dark-only, default width 1440):

| Artifact | View | State | Theme | Width |
|---|---|---|---|---|
| `mocks/task-detail.png` | `task-detail` | default | dark | 1440 |
| `mocks/task-detail.375.png` | `task-detail` | default | dark | 375 |
| `mocks/tasks-list.empty.png` | `tasks-list` | empty | dark | 1440 |
| `mocks/run-detail.error.dark.1440.webp` | `run-detail` | error | dark | 1440 |

Parts are recognised by value, not position, after the view id: a state word, a declared
theme name, or an integer. A part that is none of these, or appears twice, is a name error.
Views whose ids contain a state or theme word are refused at manifest load, so a name never
parses two ways.

The same view at several states, themes or widths is several mocks. A mock that shows one
panel of a page names a view whose manifest entry `clip`s to that panel (§3).

### 2.3 Validation at admission

A `verify-mock-ui` task is checked when it is promoted to `backlog` and again at delivery
admission, against the manifest at the delivery's base revision:

| Refusal | Cause |
|---|---|
| `no_view_manifest` | `visual_review.manifests` is empty. |
| `no_mocks` | The task has no `mocks/` artifact. |
| `invalid_mock_name` | A name does not parse (§2.2), naming the artifact and the part. |
| `unknown_mock_view` | The view id is not in the manifest. |
| `undeclared_mock_variant` | The state is not in the view's `states`, the theme not in its app's `themes`, or a width other than the default on a view with `responsive = false`. |
| `invalid_mock_image` | The artifact is not a byte-checked image presentation ([task artifacts §5a](../task-artifacts/2_design.md#5a-image-artifacts-presentation-and-retrieval)), for example an HTML or SVG export. |

A refusal is a readiness reason in `orbit run readiness` and on the task, naming every
failing artifact, so the person who attached the mocks can rename or re-export them. A
manifest change that removes a view a pending task's mock names surfaces as the same
refusal on that task.

### 2.4 Who writes mocks, and when they change

- **Writers.** Operators, orchestrator sessions and the pilot may write under `mocks/`. The
  implementer and the reviewers may not: an `orbit.task.artifact.put` to `mocks/` from a run
  with an implement or review purpose is refused with `mocks_reserved`. A build cannot
  replace its own specification.
- **Frozen per delivery.** `visual_capture` reads the mocks' digests at delivery admission,
  records them in `visual/capture.json`, and compares against those bytes. A mock replaced
  during a delivery takes effect from the next delivery.
- **Replacing a mock** is writing the same artifact path again. The artifact store keeps the
  replaced bytes' provenance, and `capture.json` names the digest compared, so a review can
  always be traced to the mock it used.
- **Exporting.** Mocks made as canvases or HTML are exported to PNG before attaching, at the
  width the name says. Rendering an HTML mock in the kit is an open question
  ([1_scope.md › Open questions](./1_scope.md#open-questions)).

### 2.5 Deliberate deviations

Designs change during the work. When the implementer departs from a mock on purpose (the
real data does not fit, the design rules forbid the mock's colour, the operator asked for
something else in a comment), it records the deviation instead of hiding it:

```json
{
  "schema_version": 1,
  "deviations": [{
    "mock": "mocks/task-detail.png",
    "element": "Status chip in the header",
    "region": [1120, 64, 180, 32],
    "reason": "design rules pair every status colour with a word; the mock shows colour only",
    "basis": "design_rules"
  }]
}
```

- Written by the implementer as the task artifact `mock-deviations.json`; it is a durable
  artifact, not a handoff message.
- `basis` is `acceptance_criteria`, `design_rules`, `operator` (citing a task comment id),
  `data` or `infeasible`.
- The reviewer receives the deviations (§8) and does not report a declared one as a finding.
  The verifier receives the deviation a finding touches and may reject a deviation whose
  basis does not hold, which turns it back into a finding.
- Deviations appear in the PR body beside the mock, so a reader sees what was changed and
  why.

## 3. The view manifest

The manifest says how to reach and render each view; it does not choose which views are
captured (the mocks do). It is introduced by UX rules phase 2, whose UI rules use the same
views. Orbit's core knows routes, fixtures, setup commands and selectors; it knows nothing
about a particular app.

### 3.1 Format

A manifest is a TOML file in the repository, named in `visual_review.manifests`. A
workspace with several UIs (a dashboard and a website) lists one per app, or one file with
several apps.

```toml
schema_version = 1

[app.dashboard]
# Run from the checkout under the worker sandbox. The sandbox retains host network access on
# Linux (`--share-net`) and allows network access on macOS (`(allow network*)`); the loopback
# URL below is the server address, not an egress limit.
# Must print `ORBIT_VISUAL_URL=<loopback URL>` on stdout once ready, after binding port zero.
setup = ["scripts/visual-fixture-server.sh", "{fixture}"]
setup_timeout_s = 120
themes = ["dark"]                     # every theme the app supports; the first is the default
color_scheme_invariant = true         # single-theme app: both prefers-color-scheme
                                      # captures must be pixel-identical (§7)
design_rules = ["docs/design/user-interface/specs/theme.md"]
tokens = ["crates/orbit-web/assets/dashboard/css/base.css"]  # :root custom properties

[[view]]
id = "task-detail"
app = "dashboard"
title = "Task detail"
route = "#tasks/ORB-00042"
fixture = "busy-workspace"
ready = "#task-detail-body"           # selector that must be visible before capture
responsive = true
states = ["default", "loading", "error"]
mask = []                             # selectors painted over before capture

[[view]]
id = "task-detail-long"               # same route, fixture with long titles and many runs
app = "dashboard"
title = "Task detail, long content"
route = "#tasks/ORB-00077"
fixture = "busy-workspace"
ready = "#task-detail-body"
```

Fields of a view:

| Field | Required | Meaning |
|---|---|---|
| `id` | yes | Stable, lowercase, hyphenated, and containing no state or theme word as a dot-separated part (§2.2). Used in mock names and artifact paths. |
| `app` | yes | The app whose setup, themes, rules and tokens apply. |
| `title` | yes | Human name, shown in prompts and the PR body. |
| `route` | yes | Path or hash appended to the setup URL. |
| `fixture` | yes | Name passed to setup; the app owns what it means. |
| `ready` | yes | Selector that must be visible; capture fails `render_timeout` without it (§7). |
| `responsive` | no, `true` | `false` allows only the default width. |
| `states` | no, `["default"]` | `default`, `empty` (fixture variant `<fixture>:empty`), `loading` (API responses held), `error` (API responses answered 500). |
| `clip` | no | Selector to capture instead of the viewport, for a view that is one panel of a page. |
| `mask` | no | Selectors painted a flat colour before capture: content that cannot be frozen. Each mask is listed in the reviewer's input. |

**Fixture parity.** A mock is compared with a capture of fixture data, not the mock's data.
To bring them closer, a manifest declares several views on one route with different
fixtures or fixture tasks (`task-detail` and `task-detail-long` above), and whoever attaches
a mock names the view whose data is closest. The reviewer is told data values differ (§8).

### 3.2 Who may change it

- **Read at the base revision.** A delivery cannot weaken its own checks: it cannot widen a
  mask, drop a rules file or change a view's route for itself.
- **A candidate may add.** View entries that exist in the candidate's manifest but not the
  base's are accepted for that delivery, so a change that adds a view and its mock can
  declare the view in the same diff. Changes to existing entries take effect from the next
  delivery.

### 3.3 Orbit's manifests

Proposed location: **crates/orbit-web/visual/views.toml**, with fixtures in
**crates/orbit-web/visual/fixtures/** and the setup script
**scripts/visual-fixture-server.sh**; the website's manifest beside `website/`. Orbit's
`.orbit/config.toml` sets `visual_review.manifests` to both.

First dashboard entries, chosen to cover the most edited views:

| id | Route | Fixture | States |
|---|---|---|---|
| `tasks-list` | `#tasks` | `busy-workspace` | default, empty, loading, error |
| `task-detail` | `#tasks/<fixture task>` | `busy-workspace` | default, loading, error |
| `run-detail` | run detail of a fixture run | `busy-workspace` | default, error |
| `jobs-live` | `#jobs/live` | `busy-workspace` | default, empty |
| `analytics-metrics` | `#analytics/metrics` | `busy-workspace` | default, loading |
| `analytics-implementers` | `#analytics/agents/implementers` | `busy-workspace` | default |
| `analytics-experiments` | `#analytics/experiments` | `experiments` | default, empty |
| `health-doctor` | `#diagnostics/doctor` | `busy-workspace` | default |
| `automation-routines` | `#operations/routines` | `busy-workspace` | default, empty |
| `settings` | `#config` | `busy-workspace` | default |

The dashboard is dark-only ([browser checks](../../DEVELOPMENT.md#browser-checks-on-a-prepared-host)),
so `themes = ["dark"]`, and `color_scheme_invariant = true` turns "both themes" into a check
that the light colour preference changes nothing (§7). The website gets `themes = ["light",
"dark"]`.

**Fixtures for Orbit.** `busy-workspace` is a seeded, disposable registry and workspace (tasks
in every status, runs with steps and failures, crews, routines), built the way the
`orbit-web` HTTP fixtures build theirs, and served by the real dashboard server bound to port
zero. Every stored timestamp is an offset from one fixed instant, and the server's notion of
"now" is pinned to that instant by a test-only build of the fixture server, not by a shipped
environment variable. Fixture seeds are data files (JSON) reviewed like code.

## 4. Capture

### 4.1 The capture driver

One driver serves `visual_capture`, `visual_recapture` and the implementer's
`orbit visual capture` (§5). It is a Node script shipped as an Orbit asset and run with the
kit's `node` and its pinned Playwright module, like the kit's own `verify`. Its helpers (PNG
decode and pixel comparison) are pinned in the kit's npm project, so `orbit browser-kit
install` installs them and `verify` covers them: every host compares pixels with the same
code. It runs under the worker sandbox. On Linux that sandbox retains host network access
(`--share-net`); on macOS it allows network access (`(allow network*)`). Neither limits
process egress to loopback; browser requests are restricted by each browser context (§4.2).

For a delivery, the driver:

1. Creates a detached checkout of the final candidate under the run's scratch directory.
2. Runs each needed app's `setup` with each needed fixture, and reads `ORBIT_VISUAL_URL`.
   Setup failure is a check finding (`setup_failed`, high) on every mock of that app.
3. For each mock, opens a fresh browser context at the mock's view, state, theme and width,
   and captures twice (§4.3).
4. Writes the captures, a side-by-side image per mock (mock left, capture right, scaled to a
   common height, with a label strip naming both), and `visual/capture.json` as task
   artifacts through the system writer, then stops the servers and removes the checkout.

### 4.2 Determinism

Each browser context is created with (these are the UX rules browser settings):

| Setting | Value |
|---|---|
| Viewport | the mock's width; height 900 at 1440, 812 at 375, otherwise `round(width × 10 / 16)` |
| Device scale factor | 1 |
| `colorScheme` | the theme being captured (`light` or `dark`) |
| `reducedMotion` | `reduce` |
| Timezone, locale | `UTC`, `en-US` |
| Clock | Playwright's clock fixed at the fixture instant before navigation; timers do not advance |
| Animations | an injected stylesheet sets `animation: none`, `transition: none` and `caret-color: transparent` on every element |
| Fonts | the app's self-hosted fonts (the dashboard serves Geist and Geist Mono); capture waits for `document.fonts.ready`. Fallback glyphs may differ between Linux and macOS |
| Network | Browser-context requests are loopback only; any request off loopback is blocked and recorded as a check finding |
| Ready | the view's `ready` selector visible, network idle, then two animation frames |
| Masks | the view's `mask` selectors painted a flat colour |

States are produced in the browser, not the fixture: `loading` holds every same-origin API
response until capture; `error` answers them with a 500 and a fixed body; `empty` uses the
`<fixture>:empty` seed.

### 4.3 Flake detection

Every capture is taken twice, each in a fresh context, and the two are compared pixel by
pixel. Identical: the image is stable. Different: a third capture is taken. If it equals one
of the first two, the capture is recorded `stable_after_retry`, with the differing region.
If not, it is `unstable`: its images are attached for the record, its differing regions are
listed, it is kept out of the model review, and a check finding `unstable_view` (low) is
recorded against the manifest, not the change. Both captures happen on one host in one
browser build, so this measures the page, not host differences.

### 4.4 Metadata

`visual/capture.json` (system-written, `schema_version: 1`) is the evidence of record:

```json
{
  "schema_version": 1,
  "outcome": "captured",
  "host": "hm_…",
  "kit": {"playwright": "1.x.y", "browser_revision": "…", "verified_at": "…"},
  "candidate_sha": "…",
  "manifest": {"path": "crates/orbit-web/visual/views.toml", "revision": "<base sha>", "sha256": "…"},
  "frozen_instant": "2026-01-01T12:00:00Z",
  "mocks": [{
    "artifact": "mocks/task-detail.375.png", "sha256": "…",
    "view": "task-detail", "state": "default", "theme": "dark", "width": 375,
    "capture": {
      "path": "visual/task-detail.375/capture.png",
      "side_by_side": "visual/task-detail.375/side-by-side.png",
      "commit": "…", "viewport": [375, 812], "device_scale_factor": 1,
      "fixture_sha256": "…", "captured_at": "…", "sha256": "…",
      "stability": "stable", "encoding": "png", "lossy": false
    }
  }],
  "over_budget": [],
  "not_established": []
}
```

`outcome` is `captured` or `not_established` (§6). A capture whose `commit` is not the
delivery's final candidate (or review commit, after a recapture) is refused by
`visual_settle` as `stale_capture`: screenshots cannot outlive the commit they show. A mock
whose `sha256` differs from the digest read at admission is refused as `mock_changed`.

### 4.5 Storage and compression

- **Paths.** `visual/<mock-name>/<side>.<ext>`, where `<mock-name>` is the mock's artifact
  name without `mocks/` and the extension, and `side` is `capture`, `side-by-side` or
  `recapture`. The `visual/` prefix is reserved for the system writer, as
  `explore-brief.json` is: an agent or operator write to it is refused.
- **Size.** Each image must stay under the 1 MiB artifact limit
  ([task artifacts §5a](../task-artifacts/2_design.md#5a-image-artifacts-presentation-and-retrieval)).
  Viewport captures of the dashboard are well under it (the ORB-19304 shots were 57–192 KB as
  PNG). The driver encodes PNG first; over the limit, lossless WebP; still over, WebP at
  quality 90 with `lossy: true`. If quality 90 still exceeds the limit, the driver records an
  `image_over_budget` error for that mock and names no capture images in `capture.json`, so no
  over-limit bytes are attached. Pixel comparison always runs on the decoded captures in
  scratch before encoding.
- **Count.** At most `max_mocks` mocks, each with one capture and one side-by-side.
- **Retention.** Captures are task artifacts and live as long as the task. `pr_open` puts
  each side-by-side image in the PR's screenshots section, ahead of implementer
  `screenshots/` files.

## 5. The implementer's self-check

### 5.1 `orbit visual capture`

The implementer runs the same driver against its own working tree, as often as it likes,
while it builds:

```sh
orbit visual capture --task ORB-12345                 # every mock
orbit visual capture --task ORB-12345 --mock mocks/task-detail.375.png
orbit visual capture --task ORB-12345 --out "$ORBIT_SCRATCH_DIR/visual"
```

MCP: `orbit.visual.capture` with `id`, optional `mock` and `out`.

- **Working tree, not a commit.** The driver runs `setup` in the current checkout, with
  uncommitted changes, so the implementer sees what it just wrote.
- **Same driver, same settings.** The view, state, theme, width, fixture, clock, fonts and
  masks are those `visual_capture` will use (§4), so what the implementer sees is what the
  review will see. The difference is that the self-check captures once (no flake retry) and
  writes nothing to the task.
- **Output.** For each mock, `<out>/<mock-name>/mock.png`, `capture.png` and
  `side-by-side.png`, plus `<out>/rules.json` with the workspace's UI rule results for those
  captures (§7). The command prints the paths. An implementer whose lane reads images
  (§9) reads the side-by-side images and compares them itself.
- **Not evidence of record.** Nothing is attached and nothing is recorded on the task. The
  implementer must not claim visual verification in its handoff; Orbit's capture (§4) is the
  record.
- **Availability.** It needs a verified kit on the host (which `verify-mock-ui` admission
  ensures, §6) and a manifest. Without either it fails naming the missing piece. It works
  in every `visual_review.mode`, including `off`, so a workspace can give implementers the
  self-check before it turns on review.

### 5.2 What the implementer does with it

The `verify-mock-ui` implement instructions (§14) tell it to:

1. Run `orbit visual capture` once before changing anything, to see the starting point
   beside each mock.
2. Build, then run it again; for each mock, list what the mock shows that the capture
   lacks, what the capture adds, and where layout, hierarchy, component kinds and static
   copy differ, using the same questions the reviewer gets (§8).
3. Fix, recapture, and repeat until the remaining differences are data, or deliberate
   deviations it records in `mock-deviations.json` (§2.5).
4. Fix the UI rule failures in `rules.json` for the mocked views.

This is where most drift is meant to be caught. The review (§8) then confirms the result
rather than finding most of the problems.

### 5.3 Image-capable implementers

The self-check is only useful to a model that can see the images. A `verify-mock-ui` task's
implementer is drawn from its usual pool filtered to crews whose executor declares
`image_input` (§9.1) and whose crew does not set `vision = false`. With no such crew in the
pool, the draw falls back to the unfiltered pool and records
`implementer_without_vision` on the run, so the operator can see it and the experiment can
exclude or stratify it.

## 6. Hosts, claimed and remote runs

### 6.1 Routing, and not established

`verify-mock-ui` implies the `ui` tag's admission rule: the task is admitted only on hosts
whose browser kit is verified. Capture runs where the candidate is; there is no owner-side
capture. When the host running `visual_capture` nonetheless has no verified kit (the kit
failed verification mid-window), capture records `outcome: not_established` with a reason:

| Reason | Cause |
|---|---|
| `browser_kit_missing` | No kit, or the kit has not passed `verify` since it last changed. |
| `kit_pin_mismatch` | The kit's Playwright version is not this build's pin. |
| `setup_failed` | Every app's candidate setup failed before rendering. |
| `no_vision_crew` | Capture and checks ran, but no crew can take images (§9); the review step only. |

`not_established` appears in `orbit run show`, `orbit task show`, the PR body and the
reviewer prompt. It is never rendered as a pass, and it is not a spec-compliance finding
against the implementer. With `visual_review.require_established = true` the candidate is
held, like an evidence-only gap in the review gate, until an operator releases it or the
delivery is resumed on a host with a kit.

### 6.2 Claimed runs

A claim already captures the owner's review contract. Visual review adds to it: the
`visual_review` settings, the manifest bytes and revision read at the claim base, the mocks'
digests, and the visual crews drawn by the owner. The follower runs that contract, never its
own settings.

- **Capture runs on the leaf**, which admission already sent to a kit host.
- **Images return in the handoff** as system-attributed deterministic attachments, alongside
  `capture.json`. The owner checks before accepting: every image's `sha256` matches
  `capture.json`, every `commit` is the handed-off head (or its review commit), the manifest
  and mock digests are the captured contract's, and the kit pin is this build's. A mismatch
  refuses the handoff with `visual_capture_mismatch`; the owner never relabels an image.
- **Review and verification** run on the leaf when the leaf runs the pre-landing review.
  Otherwise they run on the owner after handoff, from the returned artifacts and the owner's
  copy of the mocks.
- **A leaf without a kit** returns `not_established`.

### 6.3 Remote and cloud runs

A remote or cloud environment is a capture host like any other: it captures when it has a
verified kit at this build's pin, and returns `not_established: browser_kit_missing`
otherwise. The review step does not need the kit, so it may run wherever its crew can.

## 7. Deterministic checks

`visual_checks` runs before any model call, on the candidate captures' live pages (the
driver keeps each context open until its checks have run). Two kinds of check:

- **The workspace's UI rules.** Every [UX rule](../ux-rules/1_scope.md) with surface `ui`
  whose targets include the mock's view, run at the mock's state, theme and width. A rule's
  `fail` is a finding with the rule's severity, measurements and region. The same rules the
  scheduled routine runs; no second implementation.
- **Capture checks**, which only capture can know:

| Check | Rule | Default severity |
|---|---|---|
| `render_timeout` | The `ready` selector never became visible. | high |
| `setup_failed` | The candidate's setup failed for this app. | high |
| `off_loopback_request` | A browser request off loopback, blocked. | medium |
| `color_scheme_leak` | A `color_scheme_invariant` app whose light-preference capture differs from its dark one. | medium |
| `unstable_view` | §4.3; recorded against the manifest. | low |

Checks report in `shadow` until phase 6. A UI rule disabled in `ux_rules.disabled` is not
run here either. Check findings are passed to the reviewer as known issues, so it does not
spend output re-describing them, and they go through the same verification (§11), because a
rule can be wrong about intent.

## 8. The comparison review

The review prompt is assembled by `visual_review`'s input step from `capture.json`; the
agent does not choose what to look at.

1. **Mock and capture.** For each mock: the mock image, the candidate capture, and the
   side-by-side, labelled with their artifact paths. The canary image (§9.3) comes first.
2. **Declared deviations.** `mock-deviations.json` entries for that mock (§2.5).
3. **Known issues.** The check findings (§7).
4. **The precedence rule.** Stated verbatim in the prompt:
   1. The task's acceptance criteria win.
   2. Then the app's design rules (tokens, type, colour), read at the base revision, at most
      8 KiB each and 16 KiB together (the tag-instruction limits).
   3. Then the mock, for which elements appear, their arrangement and hierarchy, the kind of
      each component, and static copy (headings, labels, button text, empty-state prose).
   4. Not the mock: data values, dynamic text, text lengths, and pixel-exact spacing.
5. **The task.** Title, description, acceptance criteria and operator comments, so a change
   the operator asked for after the mock was made is not reported as a defect.

**The questions.** For each mock, the reviewer answers each question with `none` or one
finding per difference, each anchored to a region in the mock, a region in the capture, or
both:

1. **Missing.** What does the mock show that the capture does not? Name the element.
2. **Extra.** What does the capture show that the mock does not, which the task does not ask
   for?
3. **Arrangement.** Is any element in a different region, order, column or row than in the
   mock?
4. **Hierarchy.** Do headings, emphasis, grouping and the visual weight of elements follow
   the mock (which element reads first, which are grouped)?
5. **Component kind.** Is any element a different kind of control than the mock shows
   (a select instead of segmented buttons, a table instead of cards, a link instead of a
   button)?
6. **Static copy.** Do headings, labels, button text and fixed prose match the mock?
7. **State.** For a mock of the empty, loading or error state, is the capture in that state,
   and does it show what the mock shows?
8. **Rules conflict.** Where the capture follows the design rules and the mock does not, say
   so as `none` with a note, not as a finding.

The reviewer answers in a structured envelope (§11.1). Free text outside the answers is not
read.

## 9. Image delivery per provider

### 9.1 How images reach a model

Images do not travel in the prompt text: base64 inside text is never vision input. They are
staged as files in the review step's read-only scratch directory, and the executor gets them
to the model in one of two declared ways. The MCP route (`orbit.task.artifact.get` returning
a protocol image block) remains for interactive agents, but visual review does not rely on
it: the agent would choose whether to fetch, and whether a lane forwards an MCP image block
to the model is the lane's behaviour, not Orbit's.

An executor declares `spec.image_input`, like `inspection_args`:

```yaml
spec:
  image_input:
    mode: native_read        # or: attach_args
    formats: [png, jpeg, webp]
    max_images: 40
    # attach_args only: argv appended once per image; {path} is the staged file.
    # attach_args: ["--image", "{path}"]
```

- `native_read`: the CLI's own file-read tool presents image files to the model. The prompt
  lists the staged paths and tells the agent to read each one. This is also how an
  implementer reads its self-check images (§5).
- `attach_args`: the images are attached at launch through argv. Only scratch paths reach
  argv, never content. An `attach_args` implementer cannot attach images mid-run, so it can
  read self-check output only if the lane also has a native image read; otherwise the draw
  treats it as without vision for implementation (§5.3).
- Omitted: the executor cannot take images. Visual review draws skip its crews, and an
  explicit selection fails before launch with the non-retryable `ImageInputUnavailable`,
  naming the executor and `spec.image_input`, as `InspectionToolsUnavailable` does for
  inspection.

The model behind a lane matters too, so a crew may set `vision = false` to opt out a text-only
model on an image-capable lane. The canary (§9.3) catches a crew that claims vision and lacks
it.

### 9.2 Per provider

| Provider | Route | Status at design time |
|---|---|---|
| `claude` | `native_read`: Claude Code's Read tool presents PNG, JPEG, GIF and WebP files as images. | Expected to work; phase 1 probes it under the worker sandbox. |
| `codex` | `attach_args`: `codex exec` accepts image attachments on its command line (`--image`). | Expected to work for review; phase 1 confirms the flag on the pinned CLI version and whether a native read exists for the self-check. |
| `gemini` | `native_read` is expected (the Gemini CLI's file read returns image data). | Not verified; declared only after a probe passes. |
| `antigravity`, `grok`, `copilot` | Unknown. | No declaration; refused until probed. |
| `muse` | Unknown. The observed Muse Code contract takes the prompt from a file, has no Orbit-managed MCP, and documents no image attachment ([Muse Code CLI](../../CONFIG.md#muse-code-cli)). | No declaration; refused with `ImageInputUnavailable`. Phase 1 probes an authenticated `muse exec` for an attachment flag or an image-reading file tool. |
| `ollama`, `openai_compat` | No CLI runtime. | Unsupported, as for every activity. |

Phase 1 result ([ORB-19977], `orbit agents vision-probe` on Linux): the claude (`native_read`), codex
(`attach_args`) and muse (`attach_args` `--image`, Muse Code 1.4.14) lanes passed and ship
declarations; gemini was not authenticated on the probing host and ships none. Codex also has a
native image read (`view_image`), so a codex implementer can read self-check images. Some codex
models misread the canary on a passing lane; those crews opt out with `vision = false`.

### 9.3 Proving the model saw the images

Each review invocation receives, first, a canary image: a generated PNG showing a random
six-character code in large type, created per invocation and never written to the task. The
envelope must return the code in `canary`. A missing or wrong code refuses the result as
`image_not_seen`, a retryable failure. A crew that fails the canary twice in a row is held
for visual review like a [provider failure hold](../../CONFIG.md#provider-failure-holds),
and the probe in phase 1 is the same check run outside a delivery.

This is the one place visual review reads agent output as truth, and it is bounded: the
canary proves delivery, not judgement. Judgement is checked by verification (§11).

### 9.4 Crew selection

- **Reviewer:** drawn from `workflow.purpose_crews.visual_review`, keeping members whose
  executor declares `image_input` and whose crew does not set `vision = false`. Unset or
  empty: the complexity pool, under the same filter. No capable member: `not_established:
  no_vision_crew`, visibly.
- **Verifier:** drawn from the same filtered pool, passing over the reviewer's crew and the
  implementer's. With no other capable crew, findings stay `unverified` and nothing blocks or
  is filed (§11.3).
- The draw is seeded by task and candidate, so a retried step draws the same crews.

## 10. Budgets

Budgets are checked before dispatch from the staged images, then measured after.

- **Mocks:** `visual_review.max_mocks` (default 12) per delivery; the rest are listed
  `over_budget` in `capture.json`, the prompt and the PR body. Order: the mocks named in the
  task's acceptance criteria first, then by artifact name.
- **Images per invocation:** `visual_review.max_images` (default 32), and the executor's own
  `image_input.max_images`, whichever is lower. Each mock costs two images (mock and
  capture); the side-by-side is sent only when both would not fit. The canary is first.
- **Estimated image tokens:** `visual_review.max_image_tokens` (default 48,000). Orbit
  estimates per provider. For Claude models, Anthropic documents roughly width × height / 750
  tokens per image, with large images downscaled first, so a 1440×900 image costs about
  1,500 tokens and a 375×812 image about 400. Providers without a published rule use a
  configured per-image constant. The estimate decides inclusion; the invocation record's
  measured tokens are what the experiment reads.
- **Findings:** at most `visual_review.max_findings` (15) per review; extra findings are
  dropped and counted.
- **Verification:** findings are batched five per verifier invocation, as finding
  verification partitions today; each batch carries only the images its findings cite.
- **Time:** capture has a wall-clock limit per app (setup plus captures); the review steps
  use the activity runtime limits.

Twelve mocks at 1440 width is about 36,000 image tokens per review, under the default cap.

## 11. Findings, verification and severity

### 11.1 The finding

The reviewer returns, per finding:

| Field | Rule |
|---|---|
| `question` | 1–8 (§8), or `rule:<id>` / `check:<name>` for a check finding. |
| `mock` | The mock artifact the finding is about. |
| `title`, `detail` | Non-empty. `detail` says what the capture shows and what the mock shows. |
| `evidence` | At least one `{image, region}`: `image` is the mock or its capture as named in `capture.json`, `region` an `[x, y, w, h]` box inside it. Optionally `{location}` as `path:line` for the cause, checked at the candidate. |
| `severity` | `critical`, `high`, `medium`, `low` or `nit`, using the shared rubric ([ORB-19304]). |

`visual_settle` validates the evidence (the image is in this delivery's capture, the box
is inside it, a cited line exists) as finding verification validates `path:line` today. A
finding that fails is dropped as `invalid_evidence` and counted.

**Expected severities for mock differences.** `high`: an element or section the mock shows
is missing, a control of the wrong kind that changes how the view is used, or a view in the
wrong state. `medium`: a different arrangement or hierarchy that a user would notice, or a
static label that says something different. `low`: an extra element, or a hierarchy
difference within one panel. `nit`: wording or emphasis that differs but means the same.
`critical` is reserved for what the shared rubric calls critical, such as a view that shows
another task's data.

### 11.2 Verification

Every finding, check findings included, is verified in-line by `visual_verify` with a crew
other than the reviewer's (§9.4), before `visual_settle`. In-line, not through the
asynchronous `finding_verification_pipeline`, because a blocking finding must be settled
before the code review starts, and the images are already staged. The verifier uses the
existing verdict schema ([finding verification §3](../finding-verification/2_design.md#3-verdict-schema)),
with one extension: an evidence location may be an image region (`visual/<…>.png#x,y,w,h` or
`mocks/<…>.png#x,y,w,h`) from this delivery instead of `path:line`. It returns CONFIRMED or
REJECTED, a confidence, and for CONFIRMED a severity, which may differ from the reviewer's.
The verified severity is the one recorded, filed and scored, with `severity_source: verified`.

The verifier receives the cited images and regions, the precedence rule, the relevant rules
section, any deviation that touches the finding, and the task text. It does not receive the
reviewer's other findings. A low-confidence verdict escalates once to a crew of the next
tier's filtered pool, as finding verification does. A finding left without a valid verdict is
`unverified`.

### 11.3 What blocks and what is filed

| Verified outcome | Effect |
|---|---|
| CONFIRMED at or above `block_at` | Must be resolved before landing (§1). Recorded in `visual/review.json`; given to the code reviewer as must-address. If the candidate lands with the finding `disputed` and accepted by the operator, it is filed as a follow-up. |
| CONFIRMED below `block_at`, at or above `file_at` | Given to the code reviewer as optional fixes. When the reviewer does not fix one, `visual_settle` files a follow-up task after landing, tagged `visual-review` and related `spawned_from` the delivered task, with the mock, the capture and the side-by-side attached. |
| CONFIRMED below `file_at` | Recorded in `visual/review.json` and the PR body only. |
| REJECTED | Recorded with its reason. Not given to the code reviewer. |
| `unverified` | Recorded. Never blocks and is never filed. |

Defaults are `block_at = "high"` and `file_at = "low"`, so `medium` and `low` are filed and
`nit` is recorded. A workspace with strict rules sets `block_at = "medium"`; one trialling
the feature sets `block_at = "never"`. Config load refuses `file_at` above `block_at`.

A follow-up task is filed with the verification record it already has, written as the
`finding_verified` history entry finding verification writes, against the filed task's
fingerprint. `visual-review` joins the review finding tags.

In `shadow` mode nothing in this table happens except recording.

### 11.4 Scorecards

The comparison reviewer is a reviewer: its confirmed findings count as findings that held,
weighted 8, 4, 2, 1 and 0.25 by verified severity, and its rejected findings count as other
rejected review findings do ([agent scorecards](../agent-scorecards/2_design.md)). Records
carry the purpose `visual_review`. The implementer of a `verify-mock-ui` task is credited
for mocks with no confirmed finding at `medium` or above on the first candidate, which is
the measure of how well the self-check worked.

## 12. Measurement

### 12.1 The experiment

One experiment ([experiments](../experiments/1_scope.md)) in each workspace that runs it:

- **Population:** tasks tagged `verify-mock-ui`, excluding `no-diff-expected`.
- **Arms:** `control` with `visual_review.reviewer = false` (self-check, capture and checks
  still run); `treatment` with `visual_review.reviewer = true`. Weights 1:1.
  `visual_review.reviewer` is opted in to the config overlay allowlist in code, and the
  workspace adds it to `experiments.config_keys`.
- **Mode:** `live` in both arms, after the shadow phases have shown that capture is stable
  and the canary passes.
- **Minimum sample:** 30 tasks per arm, `max_days` 60. Mock-driven tasks are fewer than all
  `ui` tasks, so the sample is smaller and the horizon longer.

The self-check runs in both arms on purpose: it is cheap and it is meant to ship regardless.
The experiment answers whether the comparison reviewer's findings are worth their cost on top
of it.

### 12.2 Post-landing comparison

The primary metric needs a measurement that is the same in both arms. After a task in
either arm lands, a `visual_measure` run (shadow, never filing, never blocking) captures the
landed head for every mock and runs the comparison review and verification. Its confirmed
findings are the mismatches that reached the landed code. In the treatment arm this
measures what the reviewer and code reviewer left; in control, what the self-check alone
left. Its invocations are tagged `purpose: visual_measure` and excluded from the cost
guardrail.

### 12.3 Metrics

| Metric | Role | Definition |
|---|---|---|
| Mismatches on the landed head | Primary | Severity-weighted (8/4/2/1/0.25) confirmed findings of the post-landing comparison per task. Lower is better. |
| Review repairs | Secondary | Reviewer fix commits that address visual findings, and follow-up visual tasks filed. |
| UI regressions filed afterwards | Secondary | Severity-weighted `regression_from` links within 14 days of landing whose regressing change touches a mocked view. |
| Cost per task | Guardrail | All-invocation USD and tokens per task, excluding `visual_measure`. Threshold: treatment median no more than 15% above control. |
| Verified-finding rate | Guardrail (treatment) | CONFIRMED / (CONFIRMED + REJECTED) over the reviewer's findings, with a Wilson interval. Threshold: point estimate at least 0.6. |
| Lead time | Guardrail | Admission to landing. Threshold: treatment median no more than 20 minutes above control. |
| Done rate | Guardrail | Terminal `done`. Threshold: no more than 5 points below control. |

### 12.4 Ship gate

The comparison reviewer becomes the recommended default (`visual_review.reviewer = true`,
through the experiment's promotion task) when, at the horizon:

1. the sample-ratio check passes and no arm is drifted; and
2. every guardrail is inside its threshold, the verified-finding rate's Wilson lower bound is
   at least 0.5, and the canary failure rate is under 2% of invocations; and
3. the readout's verdict is `winner treatment` on the primary metric, **or** it is
   `no difference` or `inconclusive` and the treatment had at least one confirmed `medium`
   or higher finding per five tasks that the code reviewer fixed before landing.

If the gate fails on the verified-finding rate, the questions and the precedence wording are
revised and the experiment is cloned, not extended. If control's landed-head mismatches are
already near zero, the self-check is doing the work and the reviewer is not needed; the
readout says so.

## 13. Later: before/after review without mocks

Small UI changes have no mocks and get no comparison. The first version of this design
(ORB-19397, at commit `92f682b38`) specified a review for them, which becomes a later phase
filed only when mock-driven review is live and small changes are where defects escape. It
would add, on top of this design:

- **Selection by paths.** Manifest `globs` per view and per-app `shared_globs` with a smoke
  set, so a diff's changed paths select views without mocks.
- **Base capture.** A second checkout at the base revision, before and after images, and
  changed-region boxes from a pixel diff.
- **Reference views.** Views a changed view should match, captured at the candidate.
- **Rubric questions.** Scope of change outside the changed regions, spacing, label to value,
  alignment, typography, colour and status, truncation, theme, states, consistency with
  references.

The capture driver, checks, image delivery, verification, budgets and findings are shared
with this design unchanged.

## 14. Tag instructions

**`verify-mock-ui/implement.md`** (new) says:

- The mocks under `mocks/` are this task's visual specification. Each name gives the view,
  state, theme and width.
- Precedence: acceptance criteria, then the design rules, then the mock for structure,
  layout, component kinds and static copy. Data values and pixel-exact spacing in a mock are
  illustrative.
- Run `orbit visual capture --task <ID>` before you start and after each change. Read every
  `side-by-side.png` and answer the review's questions yourself: missing, extra,
  arrangement, hierarchy, component kind, static copy, state. Fix what differs.
- Fix the UI rule failures in `rules.json` for the mocked views.
- When you depart from a mock on purpose, record it in `mock-deviations.json` with its basis.
  Do not write under `mocks/`; ask in a task comment if a mock is wrong.
- Orbit captures the evidence of record on the final candidate. Do not claim visual
  verification in your handoff, and do not attach screenshots of mocked views.
- Keep the manifest current in the same diff: add a view and its fixture when a mock names a
  route or state the manifest lacks.

**`verify-mock-ui/review.md`** (new) says:

- Read `visual/capture.json` and `visual/review.json`. Confirmed findings at or above the
  blocking severity must be addressed: fix each in your commit or mark it disputed with a
  reason. Lower confirmed findings are optional fixes; unfixed ones at or above the filing
  severity are filed as follow-ups.
- Do not repeat findings already in `visual/review.json`, and do not compare mocks with
  implementer screenshots yourself.
- `not_established` with a reason is not missing evidence: do not record a spec-compliance
  finding for it.

**`ui/implement.md` and `ui/review.md`** change only to point at `verify-mock-ui` for tasks
with mocks. The workspace guide's screenshot rule and
[browser checks](../../DEVELOPMENT.md#browser-checks-on-a-prepared-host) change in phase 6:
the implementer's capture duty is dropped for mocked views of a `verify-mock-ui` task in a
workspace with visual review `live`, and remains for everything else.

## 15. Concerns & Honest Limitations

- **Mock fidelity.** Mocks made with Claude have made-up data, ideal text lengths and
  sometimes colours outside the tokens. The precedence rule and deviations exist for this,
  and false positives are watched through the verified-finding rate, but a reviewer that
  keeps reporting "the mock shows 3 rows, the capture shows 12" will be ignored. The prompt
  says data values are illustrative; whether models follow that is measured, not assumed.
- **Fixture parity is a choice.** The closest view to a mock's data is picked by whoever
  attaches it. A poor choice makes the comparison noisy. Declaring more views per route is
  cheap; picking among them is not automatic.
- **Mocks go stale.** Replacement between deliveries and declared deviations handle it, but
  both need someone to act. A mock that nobody updates after an operator comment changes the
  design produces findings the reviewer should reject, which costs a verifier batch.
- **Unproven image routes.** Only the Claude and Codex routes are expected to work, and both
  need a probe. An `attach_args`-only lane can review but cannot use the self-check. Visual
  review may launch with fewer vision crews than hoped.
- **The self-check depends on the implementer looking.** The tool produces the images; the
  instructions tell the implementer to compare. An implementer that skips it is visible
  only in the comparison findings and its scorecard credit.
- **Models compare structure well and pixels badly.** The questions ask about presence,
  arrangement, kind and copy, which models judge reliably, and leave measurements to the UI
  rules. A difference of a few pixels in a mock is neither asked about nor reported.
- **Fixtures are a second codebase.** Seeds, a frozen server clock and a setup script must be
  kept working, and a stale fixture renders an error page. `render_timeout` and the
  `console-clean` rule make that visible on the delivery that broke it.
- **Frozen clocks are partial.** Playwright fixes the browser's clock; the server's "now"
  needs the fixture server's own seam, and anything computed from wall time elsewhere must be
  masked. Masks hide real defects in the masked area.
- **Fonts and rendering across hosts.** Self-check and capture of record may run on
  different hosts in a claimed run. The comparison is mock against capture, never capture
  against capture across hosts, so this affects only what the implementer saw, slightly.
- **Small changes are uncovered** until §13 is built.
- **Disputes need a human.** A blocking finding the code reviewer disputes waits for the
  operator. If disputes are common, the questions or the precedence wording are wrong, and
  that shows in the verified-finding rate and dispute count.

## Task References

- [ORB-19397] — the first version of this design: before/after visual review
- [ORB-19304] — adds the shared five-level severity scale and weights findings and regressions by it
- [ORB-11365] — added the bounded artifact read surface and image presentation classification
- [ORB-18351] — publishes screenshot artifacts into the PR body through the task's evidence ref
- [ORB-19977] — phase 1: image-input probes
- [ORB-19978] — phase 2: mock convention
- [ORB-19979] — phase 3: capture driver and implementer self-check
- [ORB-19980] — phase 4: capture of record and checks in shadow
- [ORB-19981] — phase 5: comparison review and verification in shadow
- [ORB-19982] — phase 6: live mode and experiment
- [ORB-13989] — made the reviewer fix its findings as one second commit
- [ORB-14849] — added before-landing review of the open PR

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
