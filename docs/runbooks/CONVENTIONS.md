---
type: runbook
summary: Authoring and maintenance conventions for Orbit operational runbooks.
tags: [docs, operations, runbooks]
paths: ["docs/runbooks/**"]
related_features: [orbit-docs]
related_artifacts: []
last_validated: 2026-09-12
---

# Runbook Conventions

Runbooks under `docs/runbooks/` are command-first procedures for operating Orbit after
installation. They optimize for an operator arriving with a concrete goal or symptom, not
for explaining system design from first principles.

## 1. Scope

Create or extend a runbook when the reader needs to do one of these things:

- perform a repeatable operational task;
- diagnose a recognizable failure mode;
- recover state safely; or
- verify that a runtime surface is healthy.

Keep reference material in [`CONFIG.md`](../CONFIG.md), implementation rationale under
[`docs/design/`](../design/), and reusable code shapes under
[`docs/design-patterns/`](../design-patterns/). Host inventories, credentials, ports, unit
enablement, and machine-specific sync topology belong in a private operations knowledge base.

## 2. Files and frontmatter

- Use one operator goal or tightly coupled failure mode per file.
- Name files with lowercase hyphenated nouns or actions, such as
  `database-recovery.md` or `health-checks.md`.
- Do not create a `README.md`; [`../INDEX.md`](../INDEX.md#runbooks) is the generated index.
- `CONVENTIONS.md` is the only uppercase filename in this directory.

Every runbook uses Orbit Docs' locked frontmatter schema. `type` and `summary` are required;
the other four fields are optional. Do not add fields such as `title`, `owner`, `status`, or
`last_updated` without first changing the locked schema described by [Locked orbit-docs frontmatter schema](../design/orbit-docs/4_decisions.md#locked-orbit-docs-frontmatter-schema).

```yaml
---
type: runbook
summary: One-line retrieval hook naming the operator goal or symptom.
tags: [operations, recovery]
paths: ["crates/orbit-store/**"]
related_features: [orbit-core]
related_artifacts: ["<task-id>"]
---
```

Use `paths` only for source areas to which the procedure genuinely applies. Use
`related_artifacts` for the tasks, retained ADR references, or frictions that establish the behavior;
do not invent IDs for documentation-only edits.

## 3. Recommended structure

Start with an H1 that names the operator outcome. Follow it with a short sentence saying
when to use the runbook. Then use only the sections the procedure needs, normally in this
order:

1. **Prerequisites or safety** — required state, backups, service-stop requirements, and
   destructive effects.
2. **Inspect or diagnose** — commands that establish the actual state before mutation.
3. **Procedure** — ordered, copyable steps with placeholders explained before use.
4. **Verification** — the observable success condition and expected exit status/output.
5. **Rollback or escalation** — how to return to the prior state or what evidence to gather
   when the procedure fails.
6. **Related references** — links to configuration, design, or adjacent runbooks.

Do not add empty boilerplate sections. A read-only inspection runbook may need only an
inspection sequence and interpretation notes.

## 4. Commands and safety

- Prefer commands that can be copied as written. Mark placeholders with angle brackets and
  define them nearby.
- Establish the exact binary, config path, workspace, branch, and relevant environment
  overrides before diagnosing unexpected behavior.
- Put a warning immediately before destructive or irreversible commands. Name what will be
  deleted, overwritten, restarted, or made unavailable.
- For SQLite in WAL mode, never recommend copying only the live main database file. Stop
  writers or use a consistent snapshot mechanism.
- Show expected output only when it helps the operator distinguish success from a dangerous
  partial result. Label excerpts as examples rather than promises of byte-for-byte output.
- Prefer public CLI surfaces over direct database edits. When recovery requires `sqlite3`,
  explain why and require a backup first.
- Never embed secrets, machine-specific credentials, or private hostnames.

## 5. Maintenance and porting

- Update the runbook in the same change as behavior that makes a command, path, state
  transition, or expected output stale.
- Keep [`../INDEX.md`](../INDEX.md#runbooks) generated and short; substantive procedures
  belong here and should not be duplicated in the index.
- When splitting a large document, preserve operational detail, repair relative links, and
  replace section-number references with direct file links.
- Search the repository for incoming links before moving or renaming a runbook. Prefer a
  stable index or short redirect over silently breaking established paths.
- Validate frontmatter through the Orbit Docs surface and run the repository's documentation
  checks after substantial edits.

## 6. Section index

Do not edit [`../INDEX.md`](../INDEX.md#runbooks) by hand. Give each runbook a precise H1 and
one-line `summary`, then run `scripts/generate-doc-indexes.sh` from the repository root. Use
`scripts/generate-doc-indexes.sh --check` to verify that the top-level doc index is current.
