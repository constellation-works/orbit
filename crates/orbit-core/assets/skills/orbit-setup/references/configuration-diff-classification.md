# Configuration: landed diff classification

## Contents

- [Landed diff classification](#landed-diff-classification)

## Landed diff classification

Each landed delivery records the lines it added and deleted, and the files it
changed, in three classes shown under Analytics › Metrics › Lines changed. Each
changed path is classed in this order:

1. A path matching `workflow.test_paths` counts as **tests**.
2. Otherwise, a path matching `workflow.non_code_paths` counts as **other**.
3. Anything else counts as **code**.

Both keys are arrays of path globs with the `workflow.light_delivery_paths`
syntax: `*` stays within one path component, `**` spans components, and a
leading `!` excludes what it matches. A path passes a list when some positive
glob matches it and no `!` glob does, whatever their order. A list with no
positive glob matches nothing, so `[]` counts no path in that class.

A
`!` glob removes a path from its list only; the path then falls through to the
next class, so `!**/fixtures/**` in `test_paths` makes fixtures count as other
or code, depending on `non_code_paths`.

Unset keys use language-neutral naming conventions:

- `workflow.test_paths`: `test/`, `tests/`, `__tests__/`, `spec/` and
  `testdata/` directories at any depth, and files named `*_test.*`, `*.test.*`,
  `test_*.*`, `*_spec.*` or `*.spec.*`.
- `workflow.non_code_paths`: `docs/**`, and prose, data, configuration, lock,
  golden, image and `LICENSE*` files at any depth (for example `*.md`, `*.rst`,
  `*.json`, `*.toml`, `*.lock`, `*.png`).
- `workflow.non_reviewable_paths`: empty. Paths here are left out of the
  reviewable-lines denominator. Docs and config stay reviewable, so this is
  not the non-code class and it is not a built-in list of one toolchain's
  generated files. Set lockfiles, goldens, snapshots and generated paths
  when those lines should not count.

Most projects need no change. Set a key only when the repository's layout
differs. An array replaces the inherited value wholesale, so a workspace list
replaces the default rather than adding to it: copy any default you still want.
Use the workspace file for one repository and `--global` for every repository
on the host:

```toml
[workflow]
test_paths = ["**/tests/**", "e2e/**", "**/*_test.*"]
non_code_paths = ["docs/**", "**/*.md", "assets/**"]
```

```bash
orbit config set workflow.test_paths '["**/tests/**", "e2e/**"]'
```

A rename counts only its changed lines. A binary file counts as one changed file
with no lines.

**Inspect the effective lists.** `orbit config keys --json` confirms both keys
(`array<string>`, section `delivery`). `orbit config get workflow.test_paths`
prints the effective value, built-in default included, and
`orbit config show --all` labels configured values by source (`workspace`,
`global`, or `environment`); `workflow.default_crew` shows `environment` when
the `CONSTELLATION_DEFAULT_PROVIDER` variable supplies it (no provider
detection runs at config load).

Values from compiled-in defaults
show `default`; keys with no configured value and no compiled-in default show
`unset`. File paths appear in the `Layers (later wins)` banner or in `--json`
per-key provenance.

**Verify a change on a new delivery.** The lists are compiled when a runtime
opens the workspace, so restart long-running processes such as `orbit web serve`
and any active drain first. Then note the Lines changed totals for the current
window, let a delivery land whose files span the classes, and compare the totals
after it lands. Analytics › Metrics aggregates by bucket and does not list
individual deliveries, so expect the change in the bucket where the delivery
landed, not a row for it.

Verify against a delivery that lands after the change;
do not edit recorded rows to test it.

**Changes do not reclassify history.** A change to either list applies only to
deliveries recorded after it. Rows already recorded keep their classes, even
when the new lists would classify their files differently.

**Reviewable lines are recorded forward only.** A landed row also stores the
reviewable lines of that task's own commits: added and deleted lines, minus
pure renames and minus `workflow.non_reviewable_paths`. A row written before
that denominator existed stays unavailable. So does a row whose commits
cannot be isolated to the one task. Nothing recomputes those rows by
re-diffing a shared landing range.

`cost_per_line` and `lead_time_per_line`
leave both kinds out and count them separately per arm. A change to
`workflow.non_reviewable_paths` applies only to deliveries recorded after it.

**Backfill is not a policy change.** `orbit migrate --backfill-diff-stats
[--limit N]` applies pending migrations, then records rows only for done tasks
that landed before diff stats were recorded and have no row yet. It classifies
those rows with the lists in effect when it runs, so set the lists first if the
backfilled history should follow them. It records reviewable lines only when
the landing's commits can be isolated to that task; otherwise the new row's
denominator is unavailable.

It does not rewrite existing rows. Run
`orbit migrate` without flags to see pending migrations before applying.
