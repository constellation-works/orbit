# Keep parallel runs off one another

## Keeping parallel runs off each other

- **Reservation is the system's job, not a worker's.** There is no
  worker-callable lock tool and none should be reached for.
- **Conflict detection reads live from in-flight tasks**, not only from
  reservation records — which is why current `context_files` matter more than
  they look.
- **A selector added mid-run binds only reservations requested after it.** It
  cannot retroactively revoke a reservation a concurrent run already holds.
- **Inspect and repair stale reservations** with `orbit task locks list` and
  `orbit task locks release <reservation_id> --confirm` — never by editing
  the store. The signature to match first is in [common-failures.md](common-failures.md).

## Large tasks and hierarchy

Parent/child relations describe a backlog; they do not order execution. Every
task is admitted as a leaf on its own declared `context_files`, by its own
priority, age, and dependencies — a parent inherits nothing from its children
and reserves nothing on their behalf. To sequence work, declare dependencies.

The `epic` tag is a size hint: *one large task a top-tier crew takes on whole*.
Crew selection reads it; admission ignores it. A root that used to rely on its
children's context declares none of its own, so it reserves nothing and
`reserve_locks` refuses it — give it real `context_files` or retire it.
