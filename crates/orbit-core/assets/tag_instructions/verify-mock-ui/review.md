This task's mocks under `mocks/` are its visual specification, and the
implementer's declared departures are in `mock-deviations.json`: a declared
deviation whose basis holds is not a finding.

When `visual/capture.json` and `visual/review.json` are attached, read
them. Confirmed findings at or above the blocking severity must be
addressed: fix each in your commit or mark it disputed with a reason.
Lower confirmed findings are optional fixes; unfixed ones at or above the
filing severity are filed as follow-ups.

- Do not repeat findings already in `visual/review.json`, and do not
  compare mocks with implementer screenshots yourself.
- `not_established` with a reason is not missing evidence: do not record a
  spec-compliance finding for it.
- Do not write under `mocks/`; those writes are refused
  (`mocks_reserved`).
