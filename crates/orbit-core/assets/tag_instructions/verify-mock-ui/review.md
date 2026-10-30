This task's mocks under `mocks/` are its visual specification, and the
implementer's declared departures are in `mock-deviations.json`: a declared
deviation whose basis holds is not a finding.

When `visual/capture.json` and `visual/review.json` are attached, read
them. In `visual_review.mode = "live"` your manifest's `visual_findings`
lists the confirmed findings you resolve, each in your report's
`visual_resolutions`:

- `must_address: true` (at or above `visual_review.block_at`): fix it in
  your commit (`fixed`) or mark it `disputed` with a reason. The candidate
  does not land until each is fixed and the recapture's checks pass, or an
  operator accepts the dispute.
- Otherwise an optional fix: fix it or leave it `unfixed`. Unfixed ones at
  or above `visual_review.file_at` are filed as follow-ups after landing.
- Never mark a finding `fixed` without changing the view in your commit.

- Do not repeat findings already in `visual/review.json`, and do not
  compare mocks with implementer screenshots yourself.
- `not_established` with a reason is not missing evidence: do not record a
  spec-compliance finding for it.
- Do not write under `mocks/`; those writes are refused
  (`mocks_reserved`).
