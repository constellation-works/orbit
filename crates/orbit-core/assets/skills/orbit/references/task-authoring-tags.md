# Authoring a task: tags

Contents:

- [`no-drain` tag for orchestrator-owned work](#no-drain-tag-for-orchestrator-owned-work)
- [UI tag](#ui-tag)
- [Tags that carry instructions](#tags-that-carry-instructions)
- [Light delivery tags](#light-delivery-tags)

## `no-drain` tag for orchestrator-owned work

Tag a task `no-drain` when you file it for your own session to implement, so a
live drain does not race you for it. No `orbit run auto` drain, pull-drain
follower or automatic admission pass admits it (readiness reports
`no_drain_tag`), and drain-window auto-approval never approves it — it implies
`no-auto-approve`, though task-pilot may still assess it.

An explicit
`orbit run ship <id>` and an interactive claim still work. Remove the tag with
`orbit.task.update` (full tag list without `no-drain`) to make it drain-eligible
again.

## UI tag

Tag every task whose change a user will see (a dashboard, web pages, a docs
site, a desktop app UI) `ui`. Topical tags such as `dashboard` or `website`
may stay alongside it, but `ui` is the one tooling reads; don't invent
variants (`web-ui`, `frontend`). The tag routes work to a verified browser kit
and selects the optional PR screenshots section. It does not make screenshots
a PR-opening prerequisite. Task artifact evidence follows the workspace guide
and actual diff, not the tag.

The tag also routes the task to a host with a verified headless browser kit
(the orbit-setup browser-checks reference). Admission reads the kit every
time it considers the task. A local drain, ship discovery and `orbit run ship`
start the task only on a host whose kit is verified, and an owner hands a
pull-drain claim only to a follower that reports one.

Elsewhere the task stays
in `backlog` as `browser_kit_missing`, with the wait named in readiness,
`orbit run show` and the dashboard Drain card (`waits for a host with the
browser kit (ui)`, naming `orbit config browser-kit install`), and `orbit doctor`
warns on a host whose own kit is what the task waits for. The requirement
combines with `os:` tags and the crew's executor OS: a host must satisfy all
of them, and the OS wait is named first. Only the exact `ui` tag routes,
ignoring case.

When task-pilot finds a task's criteria or context files are user-facing UI
and the task has no `ui` tag, it adds one; QA sweeps add it to the UI
findings they file. Neither ever removes a `ui` tag someone set. The workspace
guide's task-artifact evidence rule follows the diff, not the tag. The PR
screenshots section remains optional, including for `ui` tasks.

## Tags that carry instructions

Some tags add trusted instructions to the prompts of the activities that work
on the task. Orbit ships them for `no-diff-expected` (the read-only handoff
rule, for implementers), `skip-validation` (no local validation while the
change stays light, for implementers) and `ui` (browser-kit routing and the
optional PR screenshots section).

A workspace can map any tag to a repository file per role
(implement, review, recovery) under `[workflow.tag_instructions.<tag>]`,
replace a shipped default, or switch one off ([tag instructions in the published
configuration reference](https://github.com/constellation-works/orbit/blob/main/docs/CONFIG.md#workflowtag_instructionstag--extra-instructions-for-tagged-tasks)).
QA sweeps are implement tasks tagged `qa-sweep`, so that tag's implement file
instructs QA.

The text comes only from those defaults and from the mapped file as committed
at the delivery's base revision. Nothing you write on the task supplies it:
put task-specific guidance in the description and criteria, not in a tag, and
don't invent a tag to smuggle instructions in.

Tags match ignoring case. The
instructions add to an activity's built-in rules and never override them. An
oversized or missing mapped file fails the activity by name rather than
running without it. `orbit run show` lists the tag instructions each dispatch
received.

## Light delivery tags

For a documentation or CI-only change, file the task with `skip-review`, `skip-validation`
or both:

- `skip-review`: no agent review before the PR, before landing or after
  landing, unless the delivered candidate includes a
  `security-state-review` task, which still requires review of its exact
  head before landing.
- `skip-validation`: the implementer runs no local validation, and delivery runs
  no required validation and attaches no validation logs.

Either way the PR is gated only by the required hosted checks.

Set them when you file the task, or as an operator. An agent's update, the
implementer's own included, cannot add either tag to an existing task.
A session crew (`ORBIT_AGENT_*` or the `--orchestrator` crew), CLI `--model`,
or an unadvertised tool `model` key counts as that agent, even when the
process has no agent environment.

A tag applies only while every path the final candidate changes matches
`workflow.light_delivery_paths`. By default that is documentation, CI
configuration and CI helper scripts (`orbit config show` lists the patterns).
Any other changed path makes delivery ignore
both tags and review and validate normally, and the task's history records
why.

A reviewer that does run keeps its own checks, so tag a task with both to
skip everything. Don't tag code changes in the hope they stay small: the
allow-list, not the tag, decides. For low-risk work outside those paths, a
workspace can map its own tag to a review-only skip — see
[review-skip tags](../../orbit-setup/references/configuration-tags.md#review-skip-tags).
