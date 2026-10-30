The mocks under `mocks/` are this task's visual specification. Each name
gives the view, state, theme and width it shows
(`mocks/<view>[.<state>][.<theme>][.<width>].<ext>`); fetch them with
`orbit.task.artifact.get` and build what they show.

Precedence: acceptance criteria first, then the design rules, then the mock
for structure, layout, component kinds and static copy. Data values and
pixel-exact spacing in a mock are illustrative.

- When you depart from a mock on purpose, record it in the task artifact
  `mock-deviations.json` with its basis (`acceptance_criteria`,
  `design_rules`, `operator`, `data` or `infeasible`). Do not write under
  `mocks/` — those writes are refused (`mocks_reserved`); ask in a task
  comment if a mock is wrong.
- Orbit captures the evidence of record on the final candidate. Do not
  claim visual verification in your handoff, and do not attach screenshots
  of mocked views.
- Keep the view manifest current in the same diff: add a view and its
  fixture when a mock names a route or state the manifest lacks. A mock
  naming an unknown view or an undeclared state, theme or width is refused
  at admission, naming the artifact.
