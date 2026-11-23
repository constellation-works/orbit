# Tools: evolution records

## Evolution records

Evolution records measure the cost of capability gaps. `orbit.evolution.add` records an
interactive cost event; it does not create friction or propose work. Its required `event` object
has a `kind` (`workaround`, `manual_step`,
`retry` or `wait`), one-line `what`, and nonnegative integer `minutes`, `retries` and
`slot_minutes`.

Include `wished` for a workaround or manual step. An optional kebab-case
`signature` (3–64 characters) groups costs for the same capability gap into one record; an event
without a signature is kept as a cost but does not create a gap record. Optional `mandate` and
`cycle` attributes associate the event with an assignment and cycle; `cycle` requires `mandate`.

`orbit.evolution.list` lists signatures ranked by summed minutes plus slot minutes. Filter with
`state` (`open`, `proposed`, `declined` or `resolved`) or `since` (an RFC 3339 time).
`orbit.evolution.show` displays one record, its occurrences, proposal state, suppression window
and all-time aggregates.

The matching CLI commands are `orbit evolution add --event <JSON>`
(with optional `--mandate` and `--cycle`), `orbit evolution list [--state <state>] [--since <time>]`
and `orbit evolution show <id>`. The CLI also has operator-only
`orbit evolution decline <id>` to suppress proposals for a signature.

Choose the record by the kind of improvement needed: if a fix would make the system behave as
designed, record friction; if the system needs a different or new capability and the work has a
measurable cost, record an evolution event. See [Friction](friction.md) for friction guidance.
To verify the surface, run `orbit tool list` to confirm all three tools are active and
`orbit evolution --help` to inspect the CLI commands.
