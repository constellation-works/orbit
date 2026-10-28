---
title: Visual Review — Design
owner: claude
last_updated: 2026-10-28
last_validated: 2026-10-28
status: Draft
feature: visual-review
doc_role: design
type: design
summary: Proposed design for Orbit-owned visual review — manifest-selected views captured deterministically on base and final candidate, in-browser checks, a vision crew answering rubric questions against before/after and reference views, in-line finding verification with the shared severity scale, and an experiment gating the default.
tags: [visual-review, ui, review-gate, task-artifacts, finding-verification, experiments, browser-kit]
paths: ["crates/orbit-core/assets/tag_instructions/ui/**", "crates/orbit-core/assets/jobs/task_pr_pipeline.yaml", "crates/orbit-core/assets/executors/**", "crates/orbit-web/assets/dashboard/**"]
related_features: [review-gate, task-artifacts, finding-verification, experiments, agent-scorecards, distributed-drain, user-interface]
related_artifacts: [ORB-19397, ORB-19304, ORB-11365, ORB-18351, ORB-13989, ORB-14849]
---

# Visual Review — Design

This doc specifies the mechanisms of [1_scope.md](./1_scope.md): where visual review sits in
delivery, the view manifest, capture and its determinism, hosts and claims, the deterministic
checks, the reviewer's input, image delivery per provider, budgets, findings and their
verification, measurement, and the `ui` tag instruction changes. Nothing here is built. Where
this doc names a mechanism that exists today, it links its design; everything else is a
proposal, and the configuration keys are those in
[1_scope.md › Configuration](./1_scope.md#configuration).

## 1. Placement in delivery

Visual review is one phase of the delivery job, run on the committed final candidate:

```text
implement → commit → sync_base → validate
  → visual_capture      (deterministic, browser kit: base + candidate)
  → visual_checks       (deterministic, in the same browser session's results)
  → visual_review       (agent, vision crew, read-only)          ┐ skipped when
  → visual_verify       (agent, a different vision crew)         ┘ reviewer = false
  → visual_settle       (deterministic: record, file, hand to review)
  → review_gate_admit → review (code reviewer) → review_gate_settle → review_validate
  → visual_recapture    (deterministic, only when the review commit touched matched paths)
  → push → pr_open → …
```

Under `review.timing = "before_landing"` the same phase runs on the leaf or owner that runs
the landing review, before `landing_review_gate_admit`, on the head that review will settle.

**Why a separate phase that feeds the code reviewer, not either alternative:**

- **Not folded into the code reviewer.** The code reviewer is drawn from
  `workflow.purpose_crews.pre_landing_review` for code. Making it look at screenshots either
  forces that pool to be vision-capable or silently reviews images with a lane that cannot see
  them. A separate step draws its own crew (§7) and can be measured on its own (§10).
- **Not a second review with its own verdict.** The review gate has one review per candidate
  and one reviewer fix commit ([review gate](../review-gate/1_overview.md), [ORB-13989]).
  A visual step with its own fix commit would be a second fix authority and a second
  validation round. Instead `visual_settle` hands the code reviewer the confirmed findings,
  and the code reviewer decides and fixes, as it does for its own findings.

**How a blocking finding is resolved.** The code reviewer's prompt carries every confirmed
finding at a blocking severity as a *must address* item. Its certificate records each one as
`fixed` (its fix commit changes the cause) or `disputed` (with a reason). Then:

1. When the review commit changes paths a captured view matches, `visual_recapture` captures
   those views at the review commit and reruns the checks only. There is no second model round:
   one review per candidate still holds.
2. A blocking *check* finding still present after the recapture leaves the review
   `incomplete`, with the check's measurement as the evidence gap.
3. A blocking *model* finding marked `fixed` is shown with its before, candidate and
   recaptured images in `visual/review.json`. A `disputed` one holds the candidate in review
   for the operator, who either accepts the dispute or rejects the candidate. Two crews (the
   reviewer and the verifier) confirmed it, so one crew's disagreement does not clear it.

**Without a pre-landing review layer.** With `review.timing` unset nothing can resolve a
finding before landing. Confirmed blocking findings are then filed as `high` follow-ups
(§9), the PR body lists them, and config load warns that `visual_review.mode = "live"`
without a review layer blocks nothing.

**When the phase runs.** `visual_capture` resolves views (§2). When no view matches, the
whole phase records `no_matching_views` and is skipped visibly. It never depends on the
`ui` tag: a `ui` task with no matching view gets `no_matching_views`, and an untagged task
whose diff matches a view is captured.

## 2. The view manifest

Which views to capture is workspace configuration. Orbit's core knows routes, fixtures,
setup commands and globs; it knows nothing about a particular app.

### 2.1 Format

A manifest is a TOML file in the repository, named in `visual_review.manifests`. A
workspace with several UIs (a dashboard and a docs site) lists one per app, or one file with
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
themes = ["dark"]                     # every theme the app supports
color_scheme_invariant = true         # single-theme app: both prefers-color-scheme
                                      # captures must be pixel-identical (§5)
design_rules = ["docs/design/user-interface/specs/theme.md"]
tokens = ["crates/orbit-web/assets/dashboard/css/base.css"]  # :root custom properties
# A change to these paths affects every view; capture the smoke set.
shared_globs = ["crates/orbit-web/assets/dashboard/css/base.css",
                "crates/orbit-web/assets/dashboard/css/components.css",
                "crates/orbit-web/assets/dashboard/css/shell.css",
                "crates/orbit-web/assets/dashboard/index.html",
                "crates/orbit-web/assets/dashboard/js/common.js",
                "crates/orbit-web/assets/dashboard/js/router.js"]
smoke = ["tasks-list", "task-detail", "run-detail"]

[[view]]
id = "task-detail"
app = "dashboard"
title = "Task detail"
aliases = ["task detail page", "task page"]
route = "#tasks/ORB-00042"
fixture = "busy-workspace"
ready = "#task-detail-body"           # selector that must be visible before capture
responsive = true
states = ["default", "loading", "error"]
references = ["run-detail"]
mask = []                             # selectors painted over before capture
globs = ["crates/orbit-web/assets/dashboard/js/tasks.js",
         "crates/orbit-web/assets/dashboard/js/task-detail.js",
         "crates/orbit-web/assets/dashboard/js/task-pane.js",
         "crates/orbit-web/assets/dashboard/js/task-lifecycle.js",
         "crates/orbit-web/assets/dashboard/js/task-runtime.js",
         "crates/orbit-web/assets/dashboard/css/task-detail.css",
         "crates/orbit-web/src/api/tasks/**"]
```

Fields of a view:

| Field | Required | Meaning |
|---|---|---|
| `id` | yes | Stable, lowercase, hyphenated. Used in artifact paths and task fields. |
| `app` | yes | The app whose setup, themes, rules and tokens apply. |
| `title`, `aliases` | `title` | Human names. A pilot matches "match task detail" against them (§2.3). |
| `route` | yes | Path or hash appended to the setup URL. |
| `fixture` | yes | Name passed to setup; the app owns what it means. |
| `ready` | yes | Selector that must be visible; capture fails `render_timeout` without it (§5). |
| `responsive` | no, `true` | `false` captures only the widest width and skips the 375px checks. |
| `states` | no, `["default"]` | `default`, `empty` (fixture variant `<fixture>:empty`), `loading` (API responses held), `error` (API responses answered 500). |
| `clip` | no | Selector to capture instead of the viewport, for a view that is one panel of a page. |
| `mask` | no | Selectors painted a flat colour before capture: content that cannot be frozen. Each mask is listed in the reviewer's input. |
| `references` | no | View ids captured as references whenever this view is captured (§6). |
| `globs` | yes | Paths whose change selects this view. Same glob syntax as `workflow.light_delivery_paths`. |

### 2.2 Selection

1. **Paths.** The delivery's changed paths (base to candidate: committed, including deletions
   and both sides of renames) and the task's `file:`/`dir:` selectors.
2. **Views.** A view is selected when any path matches its `globs`. A path matching an app's
   `shared_globs` selects that app's `smoke` views.
3. **Declared views.** The task field `visual_views` (ids) adds views. A pilot sets it from the
   assessment, an implementer through `orbit.task.update`. A declared id the manifest lacks
   fails `visual_capture` with `unknown_view`, naming it.
4. **Ranking.** Declared views first, then by the number of matched paths, then by id. The
   first `visual_review.max_views` are captured; the rest are recorded `over_budget` with
   their ids, so the reviewer and the PR body can say what was not looked at.

### 2.3 Who may add views and references

- **The manifest is read at the base revision.** As with tag instructions and the review-skip
  guard, a delivery cannot weaken its own checks: it cannot drop a view, widen a mask or remove
  a design-rules file for itself.
- **A candidate may add.** View entries that exist in the candidate's manifest but not the
  base's are accepted as additional views for that delivery, so a change that adds a view can
  declare it in the same diff. Changes to existing entries take effect from the next delivery.
- **The pilot** returns `visual_views` and `visual_references` in its assessment when the
  description names a view ("match task detail" resolves through `title` and `aliases`). It
  validates ids against the manifest at its pinned revision.
- **The implementer** may add ids to both fields. It cannot remove ids the pilot or operator
  set.

### 2.4 Orbit's dashboard manifest

Proposed location: **crates/orbit-web/visual/views.toml**, with fixtures in
**crates/orbit-web/visual/fixtures/** and the setup script
**scripts/visual-fixture-server.sh**. Orbit's `.orbit/config.toml` sets
`visual_review.manifests = ["crates/orbit-web/visual/views.toml"]`.

First entries, chosen to cover the most edited views and the components they share:

| id | Route | Fixture | States | Selected by (globs, abbreviated) |
|---|---|---|---|---|
| `tasks-list` | `#tasks` | `busy-workspace` | default, empty, loading, error | `js/tasks.js`, `css/tasks.css`, `api/tasks/**` |
| `task-detail` | `#tasks/<fixture task>` | `busy-workspace` | default, loading, error | `js/tasks.js`, `js/task-detail.js`, `js/task-pane.js`, `js/task-lifecycle.js`, `js/task-runtime.js`, `css/task-detail.css`, `api/tasks/**` |
| `run-detail` | run detail of a fixture run | `busy-workspace` | default, error | `js/run-detail.js`, `css/run-detail.css`, `api/runs/**` |
| `jobs-live` | `#jobs/live` | `busy-workspace` | default, empty | `js/runs.js`, `css/runs.css`, `api/jobs.rs` |
| `analytics-metrics` | `#analytics/metrics` | `busy-workspace` | default, loading | `js/metrics*.js`, `js/sparkline.js`, `api/diagnostics/**` |
| `analytics-experiments` | `#analytics/experiments` | `experiments` | default, empty | `js/experiments.js`, `js/experiment-form.js`, `css/experiments.css` |
| `health-doctor` | `#diagnostics/doctor` | `busy-workspace` | default | `js/doctor.js`, `css/health.css`, `api/doctor.rs` |
| `automation-routines` | `#operations/routines` | `busy-workspace` | default, empty | `js/operations-routines.js`, `js/operations-auto-tasks.js`, `js/automation.js`, `css/automation.css` |
| `settings` | `#config` | `busy-workspace` | default | `js/config*.js`, `css/settings.css`, `api/config.rs` |

The smoke set is `tasks-list`, `task-detail` and `run-detail`. Theme is `dark` only, because
the dashboard is dark-only ([browser checks](../../DEVELOPMENT.md#browser-checks-on-a-prepared-host)),
and `color_scheme_invariant = true` turns "both themes" into a check that the light colour
preference changes nothing (§5). The website, which has light and dark themes, gets its own
app entry later with `themes = ["light", "dark"]`.

**Fixtures for Orbit.** `busy-workspace` is a seeded, disposable registry and workspace (tasks
in every status, runs with steps and failures, crews, routines), built the way the
`orbit-web` HTTP fixtures build theirs, and served by the real dashboard server bound to port
zero. Every stored timestamp is an offset from one fixed instant, and the server's notion of
"now" is pinned to that instant by a test-only build of the fixture server, not by a shipped
environment variable. Fixture seeds are data files (JSON) reviewed like code. The `empty`
variant seeds nothing; `loading` and `error` states are produced in the browser (§3.2), so
they need no seed.

## 3. Capture

### 3.1 The capture driver

`visual_capture` is a deterministic activity. It needs a verified browser kit on the host
that runs it (§4) and runs under the worker sandbox. On Linux, that sandbox retains host
network access (`--share-net` in `base_namespace_args`); on macOS, it allows network access
(`(allow network*)` in the compiled worker profile). Neither limits process egress to
loopback. Browser requests are restricted
separately by each browser context (§3.2). It:

1. Creates two detached checkouts under the run's scratch directory: the base revision and
   the final candidate head.
2. Runs the app's `setup` in each, with the view's fixture, and reads `ORBIT_VISUAL_URL`.
   Setup failure on the base side records the before side `not_established: base_setup_failed`
   with the bounded log; on the candidate side it is a check finding (`setup_failed`, high),
   because the candidate cannot render.
3. For each selected view, state, theme and width, opens a fresh browser context and captures
   twice (§3.3).
4. Captures each selected view's references at the candidate only (§6).
5. Writes the images and `visual/capture.json` as task artifacts through the system writer,
   then stops both servers and removes the checkouts.

The driver is a Node script shipped as an Orbit asset and run with the kit's `node` and its
pinned Playwright module, like the kit's own `verify`. Its helpers (PNG decode and pixel
comparison, an accessibility rules engine) are pinned in the kit's npm project, so
`orbit browser-kit install` installs them and `verify` covers them: every host compares
pixels with the same code.

A view missing at the base (a new route) is recorded `before: absent` rather than captured as
an error page.

### 3.2 Determinism

Each browser context is created with:

| Setting | Value |
|---|---|
| Viewport | 1440×900 or 375×812 (`visual_review.widths`; heights fixed per width) |
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

**Comparable captures.** Two images are comparable when they share view, state, theme,
width, height, device scale factor, fixture digest (the seed files' SHA-256), kit pin
(Playwright version and browser revision), font digest and frozen instant. Before and after
are comparable images that differ only in the commit. An image pair that is not comparable
(the kit was upgraded between them) is never diffed; it is recaptured.

### 3.3 Flake detection

Every capture is taken twice, each in a fresh context, and the two are compared pixel by
pixel. Identical: the image is stable. Different: a third capture is taken. If it equals one
of the first two, the view is recorded `stable_after_retry`, with the differing region. If
not, the view is `unstable`: its images are attached for the record, its differing regions
are listed, it is kept out of the model review, and a check finding `unstable_view` (low) is
recorded against the manifest, not the change, so the workspace masks or fixes the source of
nondeterminism. Both captures happen on one host in one browser build, so this measures the
page, not host differences.

**Before/after difference.** For each comparable pair the driver also computes the changed
pixels and merges them into at most eight bounding boxes. A pair with no changed pixels is
`unchanged`; it is listed but not sent to the model unless the view is a declared view or a
reference, which saves tokens on smoke views a shared-path change did not move.

### 3.4 Metadata

`visual/capture.json` (system-written, `schema_version: 1`) is the evidence of record:

```json
{
  "schema_version": 1,
  "outcome": "captured",
  "host": "hm_…",
  "kit": {"playwright": "1.x.y", "browser_revision": "…", "verified_at": "…"},
  "base_sha": "…", "candidate_sha": "…",
  "manifest": {"path": "crates/orbit-web/visual/views.toml", "revision": "<base sha>", "sha256": "…"},
  "frozen_instant": "2026-01-01T12:00:00Z",
  "views": [{
    "id": "task-detail", "selected_by": ["crates/orbit-web/assets/dashboard/js/tasks.js", "crates/orbit-web/assets/dashboard/js/task-detail.js", "crates/orbit-web/assets/dashboard/js/task-pane.js", "crates/orbit-web/assets/dashboard/js/task-lifecycle.js"],
    "images": [{
      "path": "visual/task-detail/default-dark-1440-after.png",
      "side": "after", "commit": "…", "state": "default", "theme": "dark",
      "viewport": [1440, 900], "device_scale_factor": 1,
      "fixture_sha256": "…", "captured_at": "…", "sha256": "…",
      "stability": "stable", "encoding": "png", "lossy": false
    }],
    "diff": {"status": "changed", "regions": [[312, 140, 480, 96]]}
  }],
  "over_budget": ["settings"],
  "not_established": []
}
```

`outcome` is `captured`, `no_matching_views` or `not_established` (§4). An image whose
`commit` is not the delivery's final candidate (or review commit, after a recapture) is
refused by `visual_settle` as `stale_capture`: screenshots cannot outlive the commit they show.

### 3.5 Storage and compression

- **Paths.** `visual/<view>/<state>-<theme>-<width>-<side>.<ext>`, with `side` in `before`,
  `after`, `ref` and `recapture`. The `visual/` prefix is reserved for the system writer, as
  `explore-brief.json` is: an agent or operator write to it is refused.
- **Size.** Each image must stay under the 1 MiB artifact limit
  ([task artifacts §5a](../task-artifacts/2_design.md#5a-image-artifacts-presentation-and-retrieval)).
  Viewport captures of the dashboard are well under it (the ORB-19304 shots were 57–192 KB as
  PNG). The driver encodes PNG first; over the limit, lossless WebP; still over, WebP at
  quality 90 with `lossy: true`. Pixel comparison always runs on the decoded captures in
  scratch before encoding, so lossy storage never affects flake detection or the diff.
- **Count.** At most `max_views` × states × themes × widths × 2 sides, plus references. The
  dashboard's worst case with the default budget is 6 views × 1 state × 1 theme × 2 widths ×
  2 sides = 24 images; extra states are captured only for declared views.
- **Retention.** Captures are task artifacts and live as long as the task. They are not
  published to the PR evidence ref unless `pr_open` selects them for the screenshots section,
  which then prefers `after` and `before` captures of declared views over implementer
  `screenshots/` files.

## 4. Hosts without the kit, claimed and remote runs

### 4.1 Not established, never skipped

When a delivery matches a view and the host running `visual_capture` has no verified kit
(none, unverified, or changed since its last pass), capture records
`outcome: not_established` with a reason:

| Reason | Cause |
|---|---|
| `browser_kit_missing` | No kit, or the kit has not passed `verify` since it last changed. |
| `kit_pin_mismatch` | The kit's Playwright version is not this build's pin. |
| `setup_failed` | Every view's candidate setup failed before rendering (an individual view's failure is a finding instead). |
| `no_vision_crew` | Capture and checks ran, but no crew can take images (§7); the review step only. |

`not_established` appears in `orbit run show`, `orbit task show`, the PR body and the
reviewer prompt. It is never rendered as a pass, and it is not a spec-compliance finding
against the implementer, who could not have produced the evidence of record. With
`visual_review.require_established = true` the candidate is held, like an evidence-only gap
in the review gate, until an operator releases it or the delivery is resumed on a host with
a kit.

`ui`-tagged tasks already wait for a kit host at admission, so `not_established` mostly
reaches untagged tasks whose diff turned out to touch a view, and kits that fail
verification mid-window. A pilot that sees a task's selectors match a manifest view should
tag it `ui`, so admission routes it.

### 4.2 Claimed runs

A claim already captures the owner's review contract. Visual review adds to it: the
`visual_review` settings, the manifest bytes and revision read at the claim base, and the
visual crews drawn by the owner (as the review contract carries the drawn reviewer). The
follower runs that contract, never its own settings.

- **Capture runs where the candidate is**, on the leaf, which `ui` admission already sent to a
  kit host.
- **Images return in the handoff** as system-attributed deterministic attachments, alongside
  `capture.json`. The owner checks before accepting: every image's `sha256` matches
  `capture.json`, every `commit` is the handed-off head (or its review commit), the manifest
  digest is the captured contract's, and the kit pin is this build's. A mismatch refuses the
  handoff with `visual_capture_mismatch`; the owner never relabels an image.
- **Review and verification** run on the leaf when the leaf runs the pre-landing review
  (`before_pr`, or `before_landing` on a listed host). Otherwise they run on the owner after
  handoff, from the returned artifacts, so a leaf without a vision crew still gets reviewed.
- **A leaf without a kit** returns `not_established`. Owner-side capture, which would need a
  candidate checkout on the owner, is an open question
  ([1_scope.md › Open questions](./1_scope.md#open-questions)).

### 4.3 Remote and cloud runs

A remote or cloud environment is a capture host like any other: it captures when it has a
verified kit at this build's pin, and returns `not_established: browser_kit_missing`
otherwise. Artifacts reach the owner through the same handoff as a claimed leaf's. The
review step does not need the kit, so it may run wherever its crew can.

## 5. Deterministic checks

`visual_checks` runs in the capture session, on the candidate captures, before any model
call. Each failure is a finding with a default severity, the view, state, theme and width, a
region box and the measurement. Findings are deduplicated by rule, view and element.

| Check | Rule | Default severity |
|---|---|---|
| `render_timeout` | The `ready` selector never became visible. | high |
| `setup_failed` | The candidate's setup failed for this view. | high |
| `page_error` | An uncaught exception or unhandled rejection. | high |
| `console_error` | A `console.error`, or a CSP violation report. | medium |
| `failed_request` | A same-origin request answered 4xx or 5xx outside the `error` state, or any request off loopback. | medium |
| `horizontal_overflow` | At 375px on a responsive view: document `scrollWidth` greater than the viewport, or an element extending past the right edge that is not inside a horizontal scroller. | high |
| `clipped_text` | Text whose box overflows a clipping ancestor with no ellipsis, or ellipsized text with neither a `title` nor an accessible name carrying the full text. | low |
| `overlapping_text` | Two visible text boxes intersect. | medium |
| `contrast` | Text below 4.5:1 (3:1 for 18.66px bold or 24px and larger) against its effective background. Disabled controls are exempt. | medium |
| `accessible_name` | A button, link, input or image without an accessible name; a `role` with missing required attributes. | medium |
| `untokenized_color` | A computed text, background or border colour not equal to any token value from the app's `tokens` files, on an element the diff's changed CSS or JS can style. | low |
| `color_scheme_leak` | A `color_scheme_invariant` app whose light-preference capture differs from its dark one. | medium |
| `theme_unapplied` | A multi-theme app where an element's text or background colour is the same in every theme while its neighbours' change. | low |
| `unstable_view` | §3.3; recorded against the manifest. | low |

Checks report in `shadow` from phase 3 and are not used to block until their false positive
rate on recent `ui` deliveries is measured. A workspace can disable a rule per app
(`checks.disable = ["untokenized_color"]`) in the manifest, read at base like the rest.

**Before the model, not instead of it.** Check findings are passed to the reviewer as known
issues, so it does not spend output re-describing them, and they go through the same
verification (§9), because a check can be wrong about intent (a deliberately truncated
column).

## 6. What the reviewer sees

The review prompt is assembled by `visual_review`'s input step from `capture.json`; the
agent does not choose what to look at.

1. **Before and after.** For each changed view, state, theme and width: the before image,
   the after image and the changed-region boxes as coordinates. Images are labelled in the
   prompt with their artifact path, and the canary image (§7.3) comes first.
2. **References.** For each view's `references` and the task's `visual_references`, the
   reference view captured at the candidate, at the same theme and width. References are
   captured at the candidate, not the base, so they share the candidate's shared stylesheet.
3. **Known issues.** The check findings (§5).
4. **The rubric.** The app's `design_rules` files, read at the base revision, at most 8 KiB
   each and 16 KiB together (the tag-instruction limits), then Orbit's questions below. A
   workspace can add a questions file per app (`questions = "<path>"`), with the same limits.
5. **The task.** Title, description and acceptance criteria, so a change the task asked for
   is not reported as a defect.

**The questions.** The prompt asks the reviewer to answer each question for each changed view,
with `none` or one finding per defect, each anchored to an image and a region:

1. **Scope of change.** Outside the changed regions, does anything differ between before and
   after? Name the element. (A difference outside the boxes is a regression candidate.)
2. **Spacing.** In the changed region, do paddings and gaps match those of sibling elements
   of the same kind, and the token scale the rules define? Name the element and the
   neighbour it differs from.
3. **Label to value.** For each label and its value, is the distance and alignment the same
   as for other label–value pairs in the view and in the reference view?
4. **Alignment.** Do the left edges and baselines of changed elements line up with the column
   or grid of the panel they sit in?
5. **Typography.** Do titles, column heads, labels and data use the type styles the rules
   assign (for the dashboard: sentence-case titles, 11px uppercase column heads, mono only
   for IDs, metrics, timestamps and code)?
6. **Colour and status.** Is every colour from the palette, and is every status colour paired
   with a word?
7. **Truncation and overflow.** Is any text cut, wrapped badly or ellipsized where it would
   fit? Is the 375px layout usable without horizontal scrolling?
8. **Theme.** In each theme, is any surface or text drawn in the other theme's colours, or
   unreadable?
9. **States.** In the empty, loading and error captures, is the message present, written as
   prose, and placed as in the reference view's states?
10. **Consistency with references.** Compared with each reference view, list differences in
    panel padding, heading style, row height, chip and button style and control placement
    that the task does not ask for.

The reviewer answers in a structured envelope (§9.1). Free text outside the answers is not
read.

## 7. Image delivery per provider

### 7.1 How images reach a model

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
  lists the staged paths and tells the agent to read each one.
- `attach_args`: the images are attached at launch through argv. Only scratch paths reach
  argv, never content.
- Omitted: the executor cannot take images. Visual draws skip its crews, and an explicit
  selection fails before launch with the non-retryable `ImageInputUnavailable`, naming the
  executor and `spec.image_input`, as `InspectionToolsUnavailable` does for inspection.

The model behind a lane matters too, so a crew may set `vision = false` to opt out a text-only
model on an image-capable lane. The canary (§7.3) catches a crew that claims vision and lacks
it.

### 7.2 Per provider

| Provider | Route | Status at design time |
|---|---|---|
| `claude` | `native_read`: Claude Code's Read tool presents PNG, JPEG, GIF and WebP files as images. | Expected to work; phase 1 probes it under the worker sandbox. |
| `codex` | `attach_args`: `codex exec` accepts image attachments on its command line (`--image`). | Expected to work; phase 1 confirms the flag on the pinned CLI version. |
| `gemini` | `native_read` is expected (the Gemini CLI's file read returns image data). | Not verified; declared only after a probe passes. |
| `antigravity`, `grok`, `copilot` | Unknown. | No declaration; refused until probed. |
| `muse` | Unknown. The observed Muse Code contract takes the prompt from a file, has no Orbit-managed MCP, and documents no image attachment ([Muse Code CLI](../../CONFIG.md#muse-code-cli)). | No declaration; refused with `ImageInputUnavailable`. Phase 1 probes an authenticated `muse exec` for an attachment flag or an image-reading file tool; the declaration ships only with that evidence. |
| `ollama`, `openai_compat` | No CLI runtime. | Unsupported, as for every activity. |

The task this design answers asks for muse image models. The design keeps the reviewer
provider-neutral and treats Muse as the first lane to probe, rather than assuming a route the
recorded CLI contract does not show.

### 7.3 Proving the model saw the images

Each review invocation receives, first, a canary image: a generated PNG showing a random
six-character code in large type, created per invocation and never written to the task. The
envelope must return the code in `canary`. A missing or wrong code refuses the result as
`image_not_seen`, a retryable failure. A crew that fails the canary twice in a row is held
for visual review like a [provider failure hold](../../CONFIG.md#provider-failure-holds),
and the probe in phase 1 is the same check run outside a delivery.

This is the one place visual review reads agent output as truth, and it is bounded: the
canary proves delivery, not judgement. Judgement is checked by verification (§9).

### 7.4 Crew selection

- **Reviewer:** drawn from `workflow.purpose_crews.visual_review`, keeping members whose
  executor declares `image_input` and whose crew does not set `vision = false`. Unset or
  empty: the complexity pool, under the same filter. No capable member: `not_established:
  no_vision_crew`, visibly.
- **Verifier:** drawn from the same filtered pool, passing over the reviewer's crew and the
  implementer's. With no other capable crew, findings stay `unverified` and nothing blocks or
  is filed (§9.3).
- The draw is seeded by task and candidate, so a retried step draws the same crews.

## 8. Budgets

Budgets are checked before dispatch from the staged images, then measured after.

- **Images per invocation:** `visual_review.max_images` (default 32), and the executor's own
  `image_input.max_images`, whichever is lower. Order of inclusion: canary, declared views'
  after then before, changed smoke views, references, unchanged views. What does not fit is
  listed in the prompt as not shown.
- **Estimated image tokens:** `visual_review.max_image_tokens` (default 48,000). Orbit
  estimates per provider. For Claude models, Anthropic documents roughly width × height / 750
  tokens per image, with large images downscaled first, so a 1440×900 capture costs about
  1,500 tokens and a 375×812 capture about 400. Providers without a published rule use a
  configured per-image constant. The estimate decides inclusion; the invocation record's
  measured tokens are what the experiment reads.
- **Text:** rules and questions are capped at 16 KiB each (§6); the task text uses the
  existing prompt limits.
- **Findings:** at most `visual_review.max_findings` (15) per review; extra findings are
  dropped and counted, which tells the operator the rubric or the change is too broad.
- **Verification:** findings are batched five per verifier invocation, as finding
  verification partitions today; each batch carries only the images its findings cite.
- **Time:** capture has a wall-clock limit per view (setup plus captures); the review steps
  use the activity runtime limits.

Worst case at the defaults for the dashboard: 32 images, of which about 24 at 1440 width,
is roughly 40,000 image tokens per review plus up to three verifier batches, each with the
images its findings cite.

## 9. Findings, verification and severity

### 9.1 The finding

The reviewer returns, per finding:

| Field | Rule |
|---|---|
| `question` | 1–10 (§6), or `check:<rule>` for a check finding. |
| `title`, `detail` | Non-empty. `detail` says what is wrong and what it should be. |
| `evidence` | At least one `{image, region}`: `image` is an artifact path in `capture.json`, `region` an `[x, y, w, h]` box inside it. Optionally `{location}` as `path:line` for the cause, checked at the candidate. |
| `reference` | Optional: the reference image and region it should match. |
| `severity` | `critical`, `high`, `medium`, `low` or `nit`, using the shared rubric ([ORB-19304]). |

`visual_settle` validates the evidence (the image exists in this delivery's capture, the box
is inside it, a cited line exists) as finding verification validates `path:line` today. A
finding that fails is dropped as `invalid_evidence` and counted.

**Expected severities.** Most visual findings are `nit` or `low`: a padding off the scale, a
misaligned label, a colour one token away. `medium` is a real defect with a workaround
(unreadable text in one state, a layout that breaks at 375px but stays usable). `high` is a
view that does not work (content hidden or overlapping so it cannot be read or used, a broken
layout at a supported width, an error state that shows nothing). `critical` is reserved for
what the shared rubric calls critical, such as a view that shows another task's data.

### 9.2 Verification

Every finding, check findings included, is verified in-line by `visual_verify` with a crew
other than the reviewer's (§7.4), before `visual_settle`. In-line, not through the
asynchronous `finding_verification_pipeline`, because a blocking finding must be settled
before the code review starts, and the images are already staged. The verifier uses the
existing verdict schema ([finding verification §3](../finding-verification/2_design.md#3-verdict-schema)),
with one extension: an evidence location may be an image region (`visual/<…>.png#x,y,w,h`)
from this delivery's capture instead of `path:line`. It returns CONFIRMED or REJECTED, a
confidence, and for CONFIRMED a severity, which may differ from the reviewer's. The verified
severity is the one recorded, filed and scored, with `severity_source: verified`.

The verifier receives the cited images and regions, the reference image when the finding
names one, the relevant rubric section and the task text. It does not receive the reviewer's
other findings, so it judges each one on its own.

A low-confidence verdict escalates once to a crew of the next tier's filtered pool, as
finding verification does. A finding left without a valid verdict is `unverified`.

### 9.3 What blocks and what is filed

| Verified outcome | Effect |
|---|---|
| CONFIRMED `critical`, `high` (`blocking_severities`) | Must be resolved before landing (§1). Recorded in `visual/review.json`; given to the code reviewer as must-address. If the candidate lands with the finding `disputed` and accepted by the operator, it is filed as a follow-up. |
| CONFIRMED `medium`, `low` (`follow_up_severities`) | Given to the code reviewer as optional fixes. When the reviewer does not fix one, `visual_settle` files a follow-up `bug` task after landing, tagged `visual-review` and related `spawned_from` the delivered task, with its images attached. |
| CONFIRMED `nit` | Recorded in `visual/review.json` and the PR body only. Not filed. |
| REJECTED | Recorded with its reason. Not given to the code reviewer. |
| `unverified` | Recorded. Never blocks and is never filed: an unverified finding has only the reviewer's word. |

A follow-up task is filed with the verification record it already has, written as the
`finding_verified` history entry finding verification writes, against the filed task's
fingerprint. `visual-review` joins the review finding tags, so a later edit to the task
re-arms the verification gate as for any review finding.

In `shadow` mode nothing in this table happens except recording.

### 9.4 Scorecards

The vision reviewer is a reviewer: its confirmed findings count as findings that held,
weighted 8, 4, 2, 1 and 0.25 by verified severity, and its rejected findings count as other
rejected review findings do ([agent scorecards](../agent-scorecards/2_design.md)). Records
carry the purpose `visual_review`, so the reviewer scorecard can show vision review apart from
code review. A UI regression later linked to a delivery the vision reviewer passed is a
missed regression for that reviewer as well as the code reviewer, with the ORB-19304
weighting.

## 10. Measurement

### 10.1 The experiment

One experiment ([experiments](../experiments/1_scope.md)) in each workspace that runs it:

- **Population:** tasks tagged `ui`, any complexity, excluding `no-diff-expected`.
- **Arms:** `control` with `visual_review.reviewer = false`, so capture and checks still run
  and the arm differs only by the vision reviewer and its verifier; `treatment` with
  `visual_review.reviewer = true`. Weights 1:1. `visual_review.reviewer` is opted in to the
  config overlay allowlist in code, and the workspace adds it to `experiments.config_keys`.
- **Mode:** `live` in both arms (`visual_review.mode = "live"`), after the shadow phases have
  shown that capture is stable and the canary passes.
- **Minimum sample:** 40 tasks per arm, `max_days` 60.

Capture and checks are in both arms on purpose: they are cheap and deterministic, and the
question the experiment answers is whether the model's findings are worth their cost. Whether
capture and checks are worth having is read from phases 2 and 3.

### 10.2 Metrics

| Metric | Role | Definition |
|---|---|---|
| UI regressions filed afterwards | Primary | Severity-weighted (8/4/2/1/0.25) `regression_from` links to the task, filed within 14 days of landing, whose regressing change touches a manifest view's globs. Lower is better. |
| Review repairs | Secondary | Reviewer fix commits that address visual findings, and follow-up visual tasks filed against the delivery. Read as the work the reviewer surfaced, not as harm. |
| Cost per task | Guardrail | All-invocation USD and tokens per task from invocation records. Threshold: treatment median no more than 15% above control. |
| Verified-finding rate | Guardrail (treatment) | CONFIRMED / (CONFIRMED + REJECTED) over the vision reviewer's findings, with a Wilson interval. Threshold: point estimate at least 0.6. |
| Lead time | Guardrail | Admission to landing. Threshold: treatment median no more than 20 minutes above control. |
| Done rate | Guardrail | Terminal `done`. Threshold: no more than 5 points below control. |

### 10.3 Ship gate

The vision reviewer becomes the recommended default (`visual_review.reviewer = true`, through
the experiment's promotion task) when, at the horizon:

1. the sample-ratio check passes and no arm is drifted; and
2. every guardrail is inside its threshold, the verified-finding rate's Wilson lower bound is
   at least 0.5, and the canary failure rate is under 2% of invocations; and
3. either the readout's verdict is `winner treatment` on the primary metric, **or** it is
   `no difference` or `inconclusive` and the treatment found at least one confirmed `medium`
   or higher finding per ten tasks that the code reviewer fixed before landing.

The second branch exists because UI regressions are rare: forty tasks per arm will seldom
show a significant difference in them, while defects fixed before landing are observed
directly. The operator still decides; the gate is what the decision must cite.

If the gate fails on the verified-finding rate, the rubric and questions are revised and the
experiment is cloned, not extended.

## 11. Changes to the ui tag instructions

Once capture is Orbit-owned (phase 5, in the treatment arm first, by experiment instruction
overlay), the shipped defaults change as follows.

**`ui/implement.md`** says:

- Orbit captures the evidence of record after your handoff, on the final candidate and the
  base, for the views the workspace's view manifest selects. You do not need to capture or
  attach screenshots, and screenshots you attach are optional extra evidence.
- Keep the manifest current in the same diff: add a view and its fixture when you add a
  route or a state; add globs when your change affects a view no glob covers; never remove or
  narrow an existing entry to avoid a check.
- Add `visual_views` for views your change affects that globs do not select, and
  `visual_references` for views it should match.
- Use the browser kit to check your work while you build it; do not claim visual
  verification in the handoff, because Orbit records it.

**`ui/review.md`** says:

- Read `visual/capture.json` and `visual/review.json`. Confirmed `critical` and `high`
  findings must be addressed: fix each in your commit or mark it disputed with a reason.
  Confirmed `medium` and `low` findings are optional fixes; unfixed ones are filed as
  follow-ups. Do not re-file nits.
- Do not repeat findings already in `visual/review.json`, and do not file visual findings of
  your own from implementer screenshots.
- `not_established` with a reason is not missing evidence: do not record a spec-compliance
  finding for it. A view the manifest should select but did not is a manifest gap: record it
  as a `low` finding against the manifest.

The workspace guide's screenshot rule and
[browser checks](../../DEVELOPMENT.md#browser-checks-on-a-prepared-host) change in the same
phase: the implementer's capture duty becomes optional for a workspace with visual review in
`live` mode, and remains as it is for workspaces without a manifest. The `ui` tag's
admission routing to kit hosts is unchanged.

## 12. Trust boundaries

- **Repository commands.** `setup` is repository code run under the worker sandbox, with
  the same rules as required validation commands. On Linux, that sandbox retains host network
  access (`--share-net`); on macOS, it allows network access (`(allow network*)`). Neither
  limits egress to loopback. `setup` runs from the base checkout for before
  captures and the candidate checkout for after captures, so a candidate can change what its
  own server renders (that is the change under review) but not the manifest's checks, masks,
  rules or budgets, which are read at the base (§2.3). Loopback request confinement applies
  inside the browser context (§3.2), not to repository commands.
- **Images are data.** Captures are byte-checked image artifacts; nothing renders an SVG or
  HTML capture. Rendered page text that contains instructions reaches the reviewer as pixels;
  the prompt states that text in images is page content, not instructions, and the
  reviewer's envelope has no fields that act.
- **Read-only reviewers.** `visual_review` and `visual_verify` run with the reviewer
  filesystem profile and no task-update, lifecycle or dispatch tools. Their only product is
  structured findings, so they set `require_response_envelope: true`, the exception the
  workspace guide allows for read-only activities whose only product is structured data;
  `visual_settle` turns them into durable artifacts and tasks.
- **System-written artifacts.** Only the system writer writes under `visual/`. A claimed
  leaf's images are accepted only after the owner's checks (§4.2).

## 13. Concerns & Honest Limitations

- **Unproven image routes.** Only the Claude and Codex routes are expected to work, and both
  need a probe. Muse is the lane the task names, and its image route is unknown. Visual review
  may launch with fewer vision crews than hoped, and the experiment's crew mix is then narrow.
- **Models measure pixels badly.** Spacing and alignment questions ask a model for judgements
  it makes poorly at small offsets. Verification filters false positives but not misses. The
  verified-finding rate shows precision; nothing here measures recall except escaped
  regressions, which are rare and lagging.
- **Fixtures are a second codebase.** Seeds, a frozen server clock and a setup script must be
  kept working, and a stale fixture renders an error page. The `render_timeout` and
  `failed_request` checks make that visible on the delivery that broke it, but someone has to
  fix it.
- **Frozen clocks are partial.** Playwright fixes the browser's clock; the server's "now" needs
  the fixture server's own seam, and anything computed from wall time elsewhere (a host load
  reading) must be masked. Masks hide real defects in the masked area.
- **Fonts and rendering across hosts.** The kit pins the browser, and the app serves its own
  fonts, but hinting and fallback glyphs can still differ between Linux and macOS. Before and
  after are captured on one host, so the diff is unaffected; a capture compared across hosts
  is never relied on.
- **Shared-style changes are partly blind.** A change to a shared stylesheet moves the views
  and their references together, so "consistent with the reference" can pass while both
  regressed. The before/after diff and question 1 catch it, if the smoke set covers the view.
- **Cost on every UI delivery.** Capture adds minutes of wall time (two servers, double
  captures), and review adds image tokens. The budgets cap one delivery; the experiment prices
  the feature.
- **Latency of in-line verification.** Verifying before the code review adds a step to every
  delivery with findings. Batching bounds it; skipping verification for `nit` would bound it
  further and is a possible later change.
- **Disputes need a human.** A blocking finding the code reviewer disputes waits for the
  operator. If disputes are common, the rubric is wrong, and that shows in the
  verified-finding rate and dispute count.
- **Untagged UI diffs on kit-less hosts.** Visual review keys on paths, but admission routing
  keys on the `ui` tag. An untagged task that turns out to change a view reports
  `not_established` on a kit-less host. Pilot tagging reduces this; it does not remove it.

## Task References

- [ORB-19397] — this design: visual review of UI changes by Orbit-captured screenshots
- [ORB-19304] — adds the shared five-level severity scale and weights findings and regressions by it
- [ORB-11365] — added the bounded artifact read surface and image presentation classification
- [ORB-18351] — publishes screenshot artifacts into the PR body through the task's evidence ref
- [ORB-13989] — made the reviewer fix its findings as one second commit
- [ORB-14849] — added before-landing review of the open PR

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
