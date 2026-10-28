---
title: "Scope: visual review of UI changes"
owner: claude
last_updated: 2026-10-28
last_validated: 2026-10-28
status: Draft
stage: designing
next: "Daniel accepts or amends this design; implementation tasks are filed after acceptance"
feature: visual-review
doc_role: scope
type: design
summary: "Scope: Orbit captures before/after screenshots of the views a change touches on the final candidate, runs deterministic layout and accessibility checks, and has a vision-capable crew review them against reference views and a design rubric, with findings verified by a different crew"
tags: [visual-review, ui, review-gate, task-artifacts, finding-verification, experiments, browser-kit]
---

# Scope: visual review of UI changes

Status: design only, nothing built. This scope states the problem, the shape and the phasing;
[2_design.md](./2_design.md) specifies each mechanism. The feature keeps both files (rather
than the scope track's single file) because capture, checks, image delivery, findings and
measurement are separate mechanisms that each need a specification.

Bearing: a diff that changes something a user sees already attaches screenshots as task
artifacts. The `ui` tag instructions
(`crates/orbit-core/assets/tag_instructions/ui/implement.md` and `review.md`) ask the
implementer to capture every theme, a 375px width when the layout is responsive and a before
shot "when practical", and ask the reviewer to fetch them with `orbit.task.artifact.get`.
Image artifacts are byte-checked and reach a multimodal MCP client as protocol image blocks,
bounded at 1 MiB each ([task artifacts §5a](../task-artifacts/2_design.md#5a-image-artifacts-presentation-and-retrieval),
[ORB-11365]). `pr_open` publishes `screenshots/` artifacts into an optional PR section through
the task's evidence ref ([ORB-18351]). `orbit browser-kit` builds and verifies a pinned
headless Chromium per host, and `ui` tasks are admitted only on hosts whose kit is verified
([browser checks](../../DEVELOPMENT.md#browser-checks-on-a-prepared-host)). Review findings
are verified by a second crew ([finding verification](../finding-verification/2_design.md)),
and the shared severity scale `critical | high | medium | low | nit` with weights 8, 4, 2, 1
and 0.25 is being added by [ORB-19304]. Experiments assign tasks to arms and read outcomes out
per arm ([experiments](../experiments/1_scope.md)).

## Problem

Visual evidence today is whatever the implementer chose to capture, when it chose to capture
it.

- **Cherry-picked or stale.** The implementer picks the views and the moment. A shot taken
  before the last fix, or of the one state that renders well, passes as evidence. Nothing
  records the commit a screenshot came from.
- **No before, no reference.** Before shots are optional. Nothing shows the reviewer the view
  the change should match ("same layout as task detail"), so "consistent" is judged from
  memory.
- **Unknown whether the reviewer saw anything.** The code reviewer is drawn for code. Whether
  its provider passes an image to the model depends on the lane: a protocol image block
  reaches a model only through a client that forwards it, and no crew is chosen for vision.
  A review can say "screenshots look fine" without any image reaching the model.
- **"Find inconsistencies" finds little.** An open question about a whole screenshot gets an
  open answer. The workspace has concrete rules
  ([Canon Refined theme](../user-interface/specs/theme.md): tokens, column heads, sentence
  case, status colour paired with a word, 4.5:1 contrast for muted text), and no review asks
  about them.
- **Mechanical defects cost model time.** Horizontal scroll at 375px, clipped text and
  console errors can be measured in the browser, yet they are left for a model to notice or
  miss.

The fact that shapes the fix: the browser kit pins one Playwright build per Orbit release on
every host, and UI work already runs only on hosts with a verified kit. Orbit can therefore
capture the evidence itself, on the exact commit, with the same browser everywhere.

## Goal

For a delivery whose changed paths touch a view the workspace has declared, Orbit captures
the declared views itself: on the base and on the final candidate, at fixed viewports and in
every theme the app supports, with fixed fixture data, a frozen clock and animations off.
Every image records its commit, viewport, theme and capture time. Deterministic checks run
first. A vision-capable crew then answers specific questions from the workspace's design
rules, comparing before with after and the changed view with the reference views it should
match. A different crew verifies each finding, and findings carry the shared severity scale.
Confirmed `critical` and `high` findings must be resolved before landing, `medium` and `low`
are filed as follow-ups, and `nit` is recorded only. An experiment on `ui` tasks decides
whether the vision reviewer becomes the default.

Unchanged: the code review gate keeps its one review per candidate and its two-commit shape.
The implementer may still attach its own screenshots, which become optional extra evidence.
A workspace that declares no views, or keeps `visual_review.mode = "off"`, behaves exactly
as today.

## Shape

The mechanisms, each specified in [2_design.md](./2_design.md):

| Mechanism | What it does | Design |
|---|---|---|
| Placement | A `visual_review` phase after `validate` and before the code review step. Its settled result is an input to the code reviewer, which keeps the only fix commit. | [§1](./2_design.md#1-placement-in-delivery) |
| View manifest | Workspace-owned TOML: views with routes, fixtures, setup, states, themes and file globs. Changed paths and task selectors pick views; a pilot or implementer can add views and references. | [§2](./2_design.md#2-the-view-manifest) |
| Capture | Deterministic Playwright capture from the browser kit: 1440×900 and 375×812, every declared theme, frozen clock, animations and caret off, bundled fonts, double capture with a pixel diff. Base and final candidate. Metadata in a system-written `visual/capture.json`. | [§3](./2_design.md#3-capture) |
| Hosts and claims | No verified kit means `not_established` with a reason, shown wherever the review would be. Claimed leaves capture and return the images as system-attributed handoff artifacts; the owner can capture instead. | [§4](./2_design.md#4-hosts-without-the-kit-claimed-and-remote-runs) |
| Checks | In-browser checks before any model call: overflow at 375px, clipped text, contrast and labels, untokenized colours, theme leakage, console and page errors, failed requests, render timeout. Each failure is a finding. | [§5](./2_design.md#5-deterministic-checks) |
| Reviewer input | Before and after per view, theme and width; reference views captured at the candidate; changed-region boxes; the workspace rubric as numbered questions. | [§6](./2_design.md#6-what-the-reviewer-sees) |
| Image delivery | Images are staged as files and reach the model through the executor's declared `image_input` (native file read or launch-time attachment). A canary image proves the model saw them. A crew without the declaration is refused before launch. | [§7](./2_design.md#7-image-delivery-per-provider) |
| Budgets | Caps on views, images, estimated image tokens and findings, checked before dispatch; measured cost recorded per run. | [§8](./2_design.md#8-budgets) |
| Findings | Region-anchored findings with the shared severity scale, verified in-line by a different vision crew; blocking versus follow-up severities; reviewer scorecard credit. | [§9](./2_design.md#9-findings-verification-and-severity) |
| Measurement | An experiment on `ui` tasks with and without the vision reviewer, and the ship gate for making it the default. | [§10](./2_design.md#10-measurement) |
| Tag instructions | What `ui` implement and review instructions say once capture is Orbit-owned. | [§11](./2_design.md#11-changes-to-the-ui-tag-instructions) |

### Configuration

Proposed keys. Every key is read by the owner and captured into a claim's contract, like the
review contract.

| Key | Default | Meaning |
|---|---|---|
| `visual_review.mode` | `"off"` | `off`; `shadow` (capture, check and review, record everything, block nothing, file nothing); `live`. |
| `visual_review.reviewer` | `true` | Run the vision reviewer and its verifier. `false` keeps capture and checks only. Opted in as an experiment-overridable key. |
| `visual_review.manifests` | `[]` | Repository-relative view manifests, read at the delivery's base revision (§2). Empty means no view is declared, so nothing runs. |
| `visual_review.widths` | `[1440, 375]` | Viewport widths. Heights are fixed per width (900 and 812). |
| `visual_review.max_views` | `6` | Views captured per delivery; the rest are listed `over_budget`. |
| `visual_review.max_images` | `32` | Images sent to the reviewer, references included. |
| `visual_review.max_image_tokens` | `48000` | Estimated image tokens for one review invocation (§8). |
| `visual_review.max_findings` | `15` | Findings accepted from one review; extras are dropped with a count. |
| `visual_review.blocking_severities` | `["critical", "high"]` | Confirmed severities that must be resolved before landing. |
| `visual_review.follow_up_severities` | `["medium", "low"]` | Confirmed severities filed as follow-up tasks. Anything else is recorded only. |
| `visual_review.require_established` | `false` | `true` holds a matching delivery whose capture is `not_established` until an operator releases it or a kit host captures it. |
| `workflow.purpose_crews.visual_review` | unset | Pool for the vision reviewer and its verifier. Only crews whose executor declares `image_input` are drawn. |

### Surfaces

- **Task artifacts.** `visual/capture.json`, `visual/<view>/<state>-<theme>-<width>-<side>.png`
  and `visual/review.json`, written by the system writer under a reserved `visual/` prefix.
- **`orbit run show`.** One `Visual review:` line per delivery: views captured, checks failed,
  findings by severity and verdict, or `not established: <reason>`.
- **`orbit task show`.** The same summary, and the follow-up tasks it filed.
- **PR body.** The screenshots section prefers Orbit's captures to implementer screenshots and
  states `Visual review: not established (<reason>)` when that is the outcome.
- **Dashboard task detail.** The existing artifact preview, grouped by view with before and
  after side by side, plus the findings list. This UI is a later phase.
- **Reviewer scorecard.** Findings that held, weighted by verified severity, under the purpose
  `visual_review`.

## Non-goals

- **Pixel-perfect regression testing.** Before and after are compared to find what changed,
  not to fail a build on any pixel change. A change is supposed to change pixels.
- **Golden screenshots in the repository.** No baseline images are committed. The base
  revision, captured fresh, is the baseline, so there is nothing to update by hand.
- **A second review layer.** The vision reviewer does not get its own verdict or fix commit.
  The code reviewer keeps both (§1).
- **Native and desktop UIs.** The capture driver is the browser kit. A desktop app view is
  out of scope until it can be rendered in a browser.
- **Choosing a design system.** The rubric is built from the workspace's own design rules.
  Orbit ships generic questions, not taste.
- **Making `ui` tag routing stricter.** Tag-based routing to kit hosts stays as it is. Visual
  review keys on changed paths, so untagged UI diffs are covered or reported
  `not_established`.

## Concerns and honest limitations

- **Muse image input is not established.** The observed Muse Code contract has no documented
  image attachment and no Orbit-managed MCP. Until a probe establishes a route, Muse crews are
  refused for visual review, explicitly (§7).
- **Model judgement of pixels is weak.** Models misread small offsets and colours. The rubric
  asks for regions and comparisons, measurements come from the checks, and every finding is
  verified, but precision has to be measured (§10), not assumed.
- **Fixtures go stale.** A view whose fixture no longer matches its API renders an error
  state. The render-ready check catches it, but the manifest is one more thing a UI change
  must keep current.
- **Cost.** Images cost tokens on every UI delivery. Budgets cap one review, and the
  experiment prices the whole feature.
- **Small samples.** UI regressions are rare, so the primary metric will often be
  inconclusive. The ship gate allows for that (§10).

The full list is in [2_design.md §13](./2_design.md#13-concerns--honest-limitations).

## Phasing

1. **Image-input probes.** Executor `image_input` declarations and the canary probe for the
   Claude and Codex lanes, and a probe of Muse Code's image route. Nothing is reviewed yet;
   the output is a table of which crews can see.
2. **Manifest and capture in shadow.** The manifest format, Orbit's dashboard manifest,
   fixtures with a frozen clock, base and candidate capture with double-capture flake
   detection, `visual/capture.json`, and the `not_established` path. Claimed leaves return
   captures. Nothing blocks.
3. **Deterministic checks.** The checks, with findings recorded in shadow. Measure the false
   positive rate on recent `ui` deliveries before any check can block.
4. **Vision reviewer and in-line verification in shadow.** Review, verification and
   `visual/review.json`. Findings are recorded and scored but not filed and not given to the
   code reviewer.
5. **Experiment.** Live mode for `ui` tasks under an experiment with and without the reviewer
   (§10). Blocking and follow-up filing apply in the treatment arm. The `ui` tag instructions
   change here (§11).
6. **Default.** If the ship gate passes, `visual_review.reviewer = true` with `mode = "live"`
   becomes the recommended workspace setting through the experiment's promotion task.

## Open questions

1. **Owner-side capture for claimed runs?** A leaf without a kit could hand off and let the
   owner capture before landing. That needs a candidate checkout on the owner, which claimed
   delivery avoids today (§4).
2. **Should `medium` block?** The default files it as a follow-up. A workspace with strict
   design rules may want contrast failures on body text to block.
3. **Spacing measurements in code?** Label-to-value distance and spacing-scale checks could be
   measured in the browser from manifest-declared selectors, at the cost of per-view
   annotations. Phase 3 starts with the model asking and the checks measuring only overflow,
   clipping, contrast, colour tokens and errors.
4. **Should references be captured at the base?** References are captured at the candidate so
   the reviewer compares like with like. A change to a shared stylesheet moves the reference
   too, which hides the inconsistency it introduced everywhere at once (§6).
5. **Visual review without a pre-landing review layer?** With `review.timing` unset, nothing
   can resolve a blocking finding before landing. The design files those as `high` follow-ups
   and warns at config load; a workspace may prefer a hold.

## Acceptance (feature level)

- Given a delivery whose changed paths match a manifest view and a host with a verified kit,
  Orbit attaches base and candidate captures of that view at each configured width and declared
  theme, and `visual/capture.json` names the commit, viewport, theme, fixture digest, kit pin
  and capture time of every image.
- Two captures of the same view, state, theme, width, fixture and commit are pixel-identical,
  or the view is reported `unstable` and kept out of the model review.
- On a host without a verified kit, a matching delivery records
  `not_established: browser_kit_missing` in `run show`, `task show` and the PR body; it never
  reports a pass.
- Deterministic check failures appear as findings before any model is invoked, and a review
  whose canary is not reported back is refused as `image_not_seen`.
- A crew whose executor declares no `image_input` is never drawn for visual review, and an
  explicit selection of one fails before launch naming the executor and the field.
- Every recorded visual finding carries a severity and a verification verdict from a crew
  other than the reviewer's. Confirmed `critical` or `high` findings stop the candidate from
  landing until the code reviewer resolves them.
- An experiment on `ui` tasks reports, per arm, severity-weighted UI regressions, review
  repairs, cost per task and the verified-finding rate.

## Task References

- [ORB-19397] — this design: visual review of UI changes by Orbit-captured screenshots
- [ORB-19304] — adds the shared five-level severity scale and weights findings and regressions by it
- [ORB-11365] — added the bounded artifact read surface and image presentation classification
- [ORB-18351] — publishes screenshot artifacts into the PR body through the task's evidence ref

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
