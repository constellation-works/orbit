# Previously shipped auto-task bodies

These are unedited bundled defaults from integration history:

- `run-failure-patterns.yaml`: `33633d71a`
- `delivery-code-review.yaml`: `09e60b075`
- `security-review.yaml`: `663f2a7da` (before the later context-selector guidance)
- `qa-sweep.yaml`: `663f2a7da` (a body carrying the rendered branch placeholder)

The CLI tests change operator settings over these bodies, then exercise doctor,
workspace sync and show. Their canonical body digests are in Core's compiled
`assets/auto_tasks/body-history.json`; they do not depend on a fixture manifest
claiming Orbit wrote the edited bytes.

`sources.json` contains the unedited YAML for every entry in the compiled
history, keyed by default name and source integration revision. Regenerate it
alongside the history with `python3 scripts/update-auto-task-body-history.py`.
The all-records CLI regression writes each source as an untracked copy and
requires workspace sync to recognize and restore it, even in a shallow CI
checkout. It also exercises doctor and duplicated required tools for the
unsorted `backlog-hygiene` body at `4c84ad7a6`.
