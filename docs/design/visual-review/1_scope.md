---
title: "Scope: visual review of UI changes"
owner: claude
last_updated: 2026-10-29
last_validated: 2026-10-29
status: Draft
stage: designing
next: "UX rules phases 1-2 come first (they deliver the view manifest and capture settings); then this design's phases, all filed"
feature: visual-review
doc_role: scope
type: design
summary: "Scope: for tasks tagged verify-mock-ui, the implementer compares its build against the attached mocks while it works, then Orbit captures the mocked views on the final candidate, runs deterministic checks, and has a vision crew compare each capture with its mock, with findings verified by a different crew"
tags: [visual-review, ui, mocks, review-gate, task-artifacts, finding-verification, experiments, browser-kit]
---

# Scope: visual review of UI changes

Status: design only, nothing built. This scope states the problem, the shape and the phasing;
[2_design.md](./2_design.md) specifies each mechanism. The feature keeps both files (rather
than the scope track's single file) because the mock convention, capture, the implementer's
self-check, checks, image delivery, findings and measurement are separate mechanisms that
each need a specification.

Revision 2026-10-29: the first version reviewed before/after captures of whatever views a
diff touched, against reference views. Daniel redirected it to the way UI features are
actually shipped: a mock is attached to the task, and the implementer builds to it. This
version compares the implementation with the mock first, and gives the implementer the same
comparison while it works. Before/after review of changes without mocks is a later phase
(§ Phasing, phase 7; [2_design.md §13](./2_design.md#13-later-beforeafter-review-without-mocks)).
The same day Daniel put [UX rules](../ux-rules/1_scope.md) first: scheduled, executable UI
and CLI rules that replace most of the manual `ux-review-*` passes. UX rules introduce the
view manifest and the capture driver's browser settings, and their UI rules are this
design's deterministic checks, so this design builds on them.

Bearing: a diff that changes something a user sees already attaches screenshots as task
artifacts. The `ui` tag instructions
(`crates/orbit-core/assets/tag_instructions/ui/implement.md` and `review.md`) ask the
implementer to capture every theme, a 375px width when the layout is responsive and a before
shot "when practical", and ask the reviewer to fetch them with `orbit.task.artifact.get`.
For a large UI change, Daniel attaches mocks to the task (images made with Claude), and the
implementer builds from them. Image artifacts are byte-checked and reach a multimodal MCP
client as protocol image blocks, bounded at 1 MiB each
([task artifacts §5a](../task-artifacts/2_design.md#5a-image-artifacts-presentation-and-retrieval),
[ORB-11365]). `pr_open` publishes `screenshots/` artifacts into an optional PR section through
the task's evidence ref ([ORB-18351]). `orbit browser-kit` builds and verifies a pinned
headless Chromium per host, and `ui` tasks are admitted only on hosts whose kit is verified
([browser checks](../../DEVELOPMENT.md#browser-checks-on-a-prepared-host)). Review findings
are verified by a second crew ([finding verification](../finding-verification/2_design.md)),
and the shared severity scale `critical | high | medium | low | nit` with weights 8, 4, 2, 1
and 0.25 was added by [ORB-19304]. Experiments assign tasks to arms and read outcomes out
per arm ([experiments](../experiments/1_scope.md)).

## Problem

A mock is the specification of a UI task, and nothing checks the build against it.

- **Drift from the mock goes unseen.** The usual failure of a mock-driven UI task is not a
  regression somewhere else on the page. It is a build that differs from the mock: an element
  missing, a control of the wrong kind, a different hierarchy, a panel in another place,
  different labels. The code reviewer reads code; nothing puts the mock and the build side by
  side.
- **The implementer builds blind.** It reads the mock, writes code, and may take a
  screenshot, but it has no routine way to see its build next to the mock at the same size,
  theme and state, so it finds out late or never.
- **Evidence is cherry-picked or stale.** The implementer picks which views to capture and
  when. A shot taken before the last fix, or of the one state that renders well, passes as
  evidence. Nothing records the commit a screenshot came from.
- **Unknown whether the reviewer saw anything.** Whether a lane passes an image to the model
  depends on the provider, and no crew is chosen for vision. A review can say "screenshots
  look fine" without any image reaching the model.
- **Mechanical defects cost model time.** Horizontal scroll at 375px, clipped text and
  console errors can be measured in the browser, yet they are left for a model to notice or
  miss.

The fact that shapes the fix: the browser kit pins one Playwright build per Orbit release on
every host, and UI work already runs only on hosts with a verified kit. Orbit can therefore
capture the evidence itself, on the exact commit, with the same browser everywhere, and the
implementer can use the same capture while it works.

## Goal

For a task tagged `verify-mock-ui`, the mocks attached under `mocks/` are the visual
specification. Each mock's artifact name says which view, state, theme and width it shows.
While it works, the implementer can run the same capture Orbit will run and look at each
mock beside its build. After the final candidate is committed, Orbit captures exactly the
mocked views itself, at fixed viewports, with fixed fixture data, a frozen clock and
animations off, and every image records its commit. Deterministic checks run first. A
vision-capable crew then compares each capture with its mock: what the mock shows that the
build lacks, what the build adds, and where layout, hierarchy, components and static copy
differ. A different crew verifies each finding. Findings at or above a configurable severity
must be resolved before landing, lower ones are filed as follow-ups, and `nit` is recorded
only. An experiment on `verify-mock-ui` tasks decides whether the comparison reviewer becomes
the default.

The mock is authoritative for structure, not pixels. The task's acceptance criteria come
first, then the workspace's design rules (tokens, type, colour), then the mock (which
elements, their arrangement and hierarchy, the kind of each component, and static copy).
Data values, text lengths and pixel-exact spacing in a mock are illustrative.

Unchanged: the code review gate keeps its one review per candidate and its two-commit shape.
A task without the tag, a workspace with `visual_review.mode = "off"`, or a workspace with no
view manifest behaves exactly as today.

## Shape

The mechanisms, each specified in [2_design.md](./2_design.md):

| Mechanism | What it does | Design |
|---|---|---|
| Placement | A `visual_review` phase after `validate` and before the code review step. Its settled result is an input to the code reviewer, which keeps the only fix commit. | [§1](./2_design.md#1-placement-in-delivery) |
| Mocks | `verify-mock-ui` tag; mocks are image artifacts named `mocks/<view>[.<state>][.<theme>][.<width>].<ext>`, validated at admission against the view manifest, frozen per delivery, replaceable between deliveries by anyone but the implementer. Deliberate deviations are declared, not hidden. | [§2](./2_design.md#2-mocks) |
| View manifest | Workspace-owned TOML naming how to reach each view: route, fixture, setup, ready selector, states, themes, design rules and tokens. Mock names select views; the manifest only says how to render them. | [§3](./2_design.md#3-the-view-manifest) |
| Capture | Deterministic Playwright capture from the browser kit at the mock's width and theme, frozen clock, animations and caret off, bundled fonts, double capture with a pixel diff. Metadata in a system-written `visual/capture.json`. | [§4](./2_design.md#4-capture) |
| Implementer self-check | `orbit visual capture` renders the working tree's view for each mock and writes the mock, the capture and a side-by-side image to scratch, so the implementer sees its build next to the mock while it works. Not evidence of record. | [§5](./2_design.md#5-the-implementers-self-check) |
| Hosts and claims | `verify-mock-ui` routes like `ui`, to kit hosts. A host without a verified kit records `not_established` with a reason, never a pass. Claimed leaves capture and return the images as system-attributed handoff artifacts. | [§6](./2_design.md#6-hosts-claimed-and-remote-runs) |
| Checks | The workspace's [UX rules](../ux-rules/1_scope.md) for `ui`, run on the candidate captures before any model call, plus capture-only checks (render timeout, setup failure, instability). | [§7](./2_design.md#7-deterministic-checks) |
| Comparison review | Each mock beside its capture, the declared deviations, the check findings, the precedence rule and fixed questions; one finding per difference, anchored to regions in both images. | [§8](./2_design.md#8-the-comparison-review) |
| Image delivery | Images are staged as files and reach the model through the executor's declared `image_input`. A canary image proves the model saw them. A crew without the declaration is refused before launch. | [§9](./2_design.md#9-image-delivery-per-provider) |
| Budgets | Caps on mocks, images, estimated image tokens and findings, checked before dispatch. | [§10](./2_design.md#10-budgets) |
| Findings | Region-anchored findings with the shared severity scale, verified in-line by a different vision crew; `block_at` and `file_at` thresholds; reviewer scorecard credit. | [§11](./2_design.md#11-findings-verification-and-severity) |
| Measurement | An experiment on `verify-mock-ui` tasks with and without the comparison reviewer, measured by a post-landing comparison of the landed head; the ship gate for making it the default. | [§12](./2_design.md#12-measurement) |
| Tag instructions | What `verify-mock-ui` implement and review instructions say. | [§14](./2_design.md#14-tag-instructions) |

### Configuration

Proposed keys. Every key is read by the owner and captured into a claim's contract, like the
review contract.

| Key | Default | Meaning |
|---|---|---|
| `visual_review.mode` | `"off"` | `off`; `shadow` (capture, check and compare, record everything, block nothing, file nothing); `live`. The implementer self-check works in every mode once a manifest exists. |
| `visual_review.reviewer` | `true` | Run the comparison reviewer and its verifier. `false` keeps capture and checks only. Opted in as an experiment-overridable key. |
| `visual_review.manifests` | `[]` | Repository-relative view manifests, read at the delivery's base revision (§3). Empty means no view is declared: a `verify-mock-ui` task is refused at admission with `no_view_manifest`. |
| `visual_review.default_width` | `1440` | Width for a mock name with no width part. Heights are fixed per width (1440 → 900, 375 → 812; other widths use a 16:10 height). |
| `visual_review.max_mocks` | `12` | Mocks compared per delivery; the rest are listed `over_budget`. |
| `visual_review.max_images` | `32` | Images sent to one review invocation, canary included. |
| `visual_review.max_image_tokens` | `48000` | Estimated image tokens for one review invocation (§10). |
| `visual_review.max_findings` | `15` | Findings accepted from one review; extras are dropped with a count. |
| `visual_review.block_at` | `"high"` | Lowest confirmed severity that must be resolved before landing: `critical`, `high`, `medium`, `low`, or `"never"`. |
| `visual_review.file_at` | `"low"` | Lowest confirmed severity filed as a follow-up when not fixed; anything below is recorded only. Must not be above `block_at`. |
| `visual_review.require_established` | `false` | `true` holds a delivery whose capture is `not_established` until an operator releases it or a kit host captures it. |
| `workflow.purpose_crews.visual_review` | unset | Pool for the comparison reviewer and its verifier. Only crews whose executor declares `image_input` are drawn. |

### Surfaces

- **Task artifacts.** Inputs: `mocks/*.png|jpg|webp` (operator, orchestrator or pilot) and
  `mock-deviations.json` (implementer). Outputs: `visual/capture.json`,
  `visual/<mock-name>/<side>.png` and `visual/review.json`, written by the system writer
  under a reserved `visual/` prefix.
- **`orbit visual capture`.** The implementer's self-check (and an operator's), CLI and MCP.
- **`orbit run show`.** One `Visual review:` line per delivery: mocks compared, checks failed,
  findings by severity and verdict, or `not established: <reason>`.
- **`orbit task show`.** The same summary, and the follow-up tasks it filed.
- **PR body.** The screenshots section shows each mock beside Orbit's capture and states
  `Visual review: not established (<reason>)` when that is the outcome.
- **Dashboard task detail.** Mock and capture side by side with the findings list. This UI is
  a later phase.
- **Reviewer scorecard.** Findings that held, weighted by verified severity, under the purpose
  `visual_review`.

## Non-goals

- **Pixel-perfect conformance to a mock.** Mocks are drawn, not rendered. Data, text lengths
  and exact offsets in a mock are illustrative; no pixel diff is taken between a mock and a
  capture.
- **Golden screenshots in the repository.** No images are committed. Mocks are task
  artifacts and live with the task.
- **A second review layer.** The comparison reviewer has no verdict or fix commit of its own.
  The code reviewer keeps both (§1).
- **Owner-side capture.** Capture runs where the candidate is. `verify-mock-ui` tasks are
  routed to kit hosts like `ui` tasks; moving capture to the owner would need a candidate
  checkout there for no gain.
- **Rendering HTML mocks.** Mocks are images. A canvas or HTML mock is exported to PNG when
  it is attached; Orbit rendering it is a later option (open question 1).
- **Native and desktop UIs.** The capture driver is the browser kit.
- **Choosing a design system.** The rules come from the workspace; Orbit ships the
  comparison questions, not taste.

## Concerns and honest limitations

- **Mocks are not specifications of everything.** A mock made with Claude has made-up data,
  ideal text lengths and sometimes colours outside the tokens. The precedence rule keeps the
  reviewer from reporting correct work, but false positives are the risk most likely to make
  people ignore the check, so the verified-finding rate is a ship-gate guardrail (§12).
- **Fixture parity.** A capture whose data differs from the mock's adds noise. The manifest
  can declare several views on one route with different fixtures, and the reviewer is told
  to ignore data values, but the fixture that best matches a mock is still a choice someone
  makes (§3).
- **Mocks go stale.** A design that changes during the work is handled by replacing the mock
  between deliveries or declaring a deviation (§2.5). Neither happens by itself.
- **Small changes have no mocks.** A tweak without mocks gets no visual review until the
  before/after phase lands.
- **Muse image input is not established.** Until a probe establishes a route, Muse crews are
  refused for visual review and get no use from the self-check images (§9).
- **The self-check's effect is not measured on its own.** It runs in both experiment arms
  (§12.1), so its value is read from the comparison findings over time, not from a control.

The full list is in [2_design.md §15](./2_design.md#15-concerns--honest-limitations).

## Phasing

UX rules phases 1 and 2 come first: they deliver the view manifest, the browser-context
settings and the UI rules this design reuses.

1. **Image-input probes.** Executor `image_input` declarations and the canary probe for the
   Claude and Codex lanes, and a probe of Muse Code's image route. The output is a table of
   which crews can see. Independent of everything else; can run any time.
2. **Mock convention.** The `verify-mock-ui` tag and its routing like `ui`, the mock name
   grammar, admission validation against the view manifest, the reserved `mocks/` writer
   rule, mocks frozen per delivery, and `mock-deviations.json`.
3. **Capture driver and implementer self-check.** The deterministic capture driver (double
   capture, metadata) on the UX rules browser settings, `orbit visual capture` (CLI and
   MCP), the side-by-side image, the `verify-mock-ui` implement instructions telling the
   implementer to use it, and routing of `verify-mock-ui` implementers to image-capable
   crews. Nothing is reviewed by Orbit yet; the implementer sees its build beside the mock
   from here on.
4. **Capture of record and checks in shadow.** `visual_capture` and `visual_checks` in the
   delivery job on the final candidate, `visual/capture.json`, the `not_established` path,
   claimed-leaf capture, and the PR body section.
5. **Comparison review and in-line verification in shadow.** Review, verification and
   `visual/review.json`. Findings are recorded and scored but not filed and not given to the
   code reviewer.
6. **Live and experiment.** `block_at` and `file_at` take effect in the treatment arm; the
   code reviewer receives must-address findings; `visual_recapture` after the review commit;
   the post-landing measurement comparison; the experiment (§12). If the ship gate passes,
   `visual_review.reviewer = true` with `mode = "live"` becomes the recommended setting
   through the experiment's promotion task.
7. **Later.** Each filed only when needed: before/after review of UI changes without mocks
   (§13), Orbit rendering HTML mocks, and the dashboard side-by-side view.

## Open questions

1. **Render HTML mocks?** Exporting a canvas to PNG at attach time is simple but fixes the
   mock's width and theme. Rendering an HTML mock in the kit would give every width and theme
   on demand, at the cost of running untrusted markup in the capture sandbox.
2. **Visual review without a pre-landing review layer?** With `review.timing` unset, nothing
   can resolve a blocking finding before landing. The design files those as `high` follow-ups
   and warns at config load; a workspace may prefer a hold.

Resolved 2026-10-29: owner-side capture for claimed runs is not pursued (capture belongs
where the candidate is, and `verify-mock-ui` routes to kit hosts); the blocking severity is
configurable (`visual_review.block_at`).

## Acceptance (feature level)

- A `verify-mock-ui` task with a mock whose name does not parse, or names a view, state,
  theme or width the manifest does not declare, is refused at admission naming the artifact
  and the reason.
- On a kit host, `orbit visual capture --task <ID>` in the implementer's worktree writes, for
  each mock, the capture of the uncommitted working tree and a side-by-side image with the
  mock, to scratch, and attaches nothing.
- Given a delivery of a `verify-mock-ui` task on a kit host, Orbit attaches a capture of the
  final candidate for every mock, at the mock's view, state, theme and width, and
  `visual/capture.json` names the commit, viewport, theme, fixture digest, kit pin, capture
  time and the mock's digest for every image.
- Two captures of the same view, state, theme, width, fixture and commit are pixel-identical,
  or the mock is reported `unstable` and kept out of the model review.
- On a host without a verified kit, the delivery records `not_established:
  browser_kit_missing` in `run show`, `task show` and the PR body; it never reports a pass.
- A crew whose executor declares no `image_input` is never drawn for visual review, and an
  explicit selection of one fails before launch naming the executor and the field; a review
  whose canary is not reported back is refused as `image_not_seen`.
- Every recorded comparison finding carries a severity and a verification verdict from a crew
  other than the reviewer's. Confirmed findings at or above `visual_review.block_at` stop the
  candidate from landing until the code reviewer resolves them.
- An experiment on `verify-mock-ui` tasks reports, per arm, severity-weighted mock
  mismatches on the landed head, review repairs, cost per task and the verified-finding rate.

## Task References

- [ORB-19397] — the first version of this design: before/after visual review
- [ORB-19304] — adds the shared five-level severity scale and weights findings and regressions by it
- [ORB-11365] — added the bounded artifact read surface and image presentation classification
- [ORB-18351] — publishes screenshot artifacts into the PR body through the task's evidence ref

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
