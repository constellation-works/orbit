# Executing a task: agent-filled PR sections

## Agent-filled PR sections

When the workspace configures `pr.agent_sections` or `pr.required_sections`,
the `agent_implement` instruction lists the headings to fill. Optional agent
sections may be left to the template; required sections must have content.
When neither setting adds a heading, no section artifact is needed.

Write `.orbit/tmp/pr-sections.json` with exactly this top-level shape, using
one entry per configured heading. Each value is the Markdown for that section
without its heading:

```json
{
  "sections": {
    "<heading>": "<markdown>"
  }
}
```

The final execution summary or response-envelope prose is not section content;
for a single-task delivery, `pr_open` reads the task artifact. In owner mode,
attach the file before PR creation:

```bash
orbit tool run orbit.task.artifact --input '{"action":"put","id":"<task-id>","source_path":".orbit/tmp/pr-sections.json","path":"pr-sections.json"}'
```

In claimed mode, return it in `task_artifacts` so the owner durably attaches it:

```json
{
  "task_artifacts": [
    {"path": "pr-sections.json", "source_path": ".orbit/tmp/pr-sections.json"}
  ]
}
```

The rendered body matches configured names to Markdown ATX headings after
trimming and case-insensitively; heading-like lines inside fenced code do not
match. In a body template, filled content replaces the text under its heading
until the next heading at the same or a higher level.

A required section whose
heading is absent from the template is not checked. Orbit's built-in body
instead appends configured sections under `## <heading>` headings. Empty
optional sections keep the template text, or are omitted from the built-in
body.

When a checked required section has no content, it is rendered with a “Missing
required section” note. The task history records a `pr_section_gap` event
naming the gap and the configured `pr.missing_required_section` policy drafts
the PR or refuses to open it. Verify the resulting PR body and, when required
content is missing, the task history and delivery outcome.

A reused draft is
marked ready only when no required section is still
missing, and one of these recovery paths applies: its live body exactly matches
the body Orbit created for a typed `pr_section_gap` publication; a matching
body refresh was reserved before an interrupted ready transition; or, when no
typed publication exists for the head branch, a legacy pre-typed draft has
Orbit's missing-section note under a required heading outside fenced code.

A
human edit to a typed publication's body removes its exact-body evidence, and
an edit to a reserved refresh body invalidates the reservation. A legacy body
can still be recognized if an edit retains the note. Check its
`pr_draft_recovery` history record; do not assume a resumed delivery changed
the draft state.
