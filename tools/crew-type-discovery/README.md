# Crew type discovery (offline)

A standalone, read-only report that suggests candidate
`workflow.crew_performance_types` entries from implementer evidence.
It never changes draws, pools, weights or configuration, and it is
not part of the workspace build: it is a plain Python 3 script with
no dependencies, so `cargo build` at the workspace root does not
build it and no ML crate enters the `orbit` binary.

## Run

```sh
orbit agents scorecard --role implementers --per-task --json > export.json
python3 tools/crew-type-discovery/crew_type_discovery.py export.json
# or: cat export.json | python3 tools/crew-type-discovery/crew_type_discovery.py
python3 tools/crew-type-discovery/crew_type_discovery.py --json export.json
```

## How to read it

For each group (by default crew, model and tier; `--group-by tier`
groups by tier alone) the script fits a shallow regression tree
(hand-rolled variance reduction, `--max-depth 3`) on each task's
score relative to its crew's own tier mean. Features are the task's
tags and its top-level context-file path prefixes.

Each leaf is printed with its `n`, its mean relative score, and the
tag/path predicate that reaches it. A leaf becomes a suggested type
only when it has at least `--min-leaf` tasks (default 30, the
scorecard's sparse-cell floor), its mean differs from its parent
by at least `--margin` points (default 8), **and** its predicate is
exactly one positive tag or path with no exclusions. That last rule
is what the configuration matcher can express: a type matches a task
carrying any listed tag or path, with no negation, so a leaf like
`tag a and tag b` (conjunction) or `tag a and not tag b`
(exclusion) describes a different population than the pasted block
would match. Such leaves stay visible as analysis only — the text
report lists them under `Leaves with no equivalent ready-to-paste
type`, and the JSON report carries them in `unsupported` with a
`reason` — and are never labeled as equivalent ready-to-paste
configuration. Suggestions are printed
as a ready-to-paste `[[workflow.crew_performance_types]]` TOML block
for the operator to accept or ignore; nothing is applied.

## Fixtures

`fixtures/planted.json` is a synthetic export with a planted `ui`
split (+20 for one crew); the report prints that leaf and emits a
`ui` type. `fixtures/no-effect.json` has no planted effect and
yields no suggestion:

```sh
python3 tools/crew-type-discovery/crew_type_discovery.py fixtures/planted.json
python3 tools/crew-type-discovery/crew_type_discovery.py fixtures/no-effect.json
```
