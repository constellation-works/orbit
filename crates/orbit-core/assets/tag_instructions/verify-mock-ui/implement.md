The mocks under `mocks/` are this task's visual specification. Each name
gives the view, state, theme and width it shows
(`mocks/<view>[.<state>][.<theme>][.<width>].<ext>`); fetch them with
`orbit.task.artifact.get` and build what they show.

Precedence: acceptance criteria first, then the design rules, then the mock
for structure, layout, component kinds and static copy. Data values and
pixel-exact spacing in a mock are illustrative.

Run the self-check against your own working tree — the same capture the
review will compare, before the review runs it:

1. Before you change anything, run
   `orbit visual capture --task <ID> --out "$ORBIT_SCRATCH_DIR/visual"` once
   to see the starting point beside each mock.
2. Build, then run it again. Read every `side-by-side.png` (mock left, your
   capture right) and answer the review's questions yourself for each mock:
   what is missing, what is extra, and where arrangement, hierarchy,
   component kinds, static copy or the shown state differ.
3. Fix, recapture, and repeat until the remaining differences are data —
   which the mock only illustrates — or deliberate deviations you record in
   `mock-deviations.json`.
4. Fix the UI rule failures in the capture output's `rules.json` for the
   mocked views.

The self-check attaches nothing and records nothing on the task. If it
fails naming a missing browser kit or view manifest, say so in your
handoff instead of claiming you compared the mocks.

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
