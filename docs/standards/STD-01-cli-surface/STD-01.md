---
id: STD-01-cli-surface
title: CLI surface — command grammar, inputs and effects, output modes, errors, help, and golden-tested public surfaces
summary: Normative rules for any constellation CLI's user-facing surface — noun-verb grammar, honoring every input or rejecting it, read-only commands that never write, human vs `--json` output, stdout/stderr split, tables, color/TTY, errors and exit codes, help, goldens, deprecation, no internal IDs; adopt at a pinned version in every new CLI.
status: active
tags: [standard, cli, output, ux, help, errors, deprecation]
version: 2
created: 2026-09-26
updated: 2026-09-26
last_validated: 2026-09-26
related: [STD-02-rust-architecture-and-errors, STD-03-concurrency-and-process-safety, STD-04-testing-and-verification, STD-05-security-boundaries]
---

# CLI surface — command grammar, inputs and effects, output modes, errors, help, and golden-tested public surfaces

A CLI has three readers: a human scanning a terminal, a script parsing a pipe, and an
agent reading through a shell tool. The human reader notices a broken column and a script
fails loudly on bad JSON, but an agent carries on with a value it misread. These rules
detect which reader is present and give it the right bytes. They make the machine-facing
surface a contract that tests pin down, and they make every input either take effect or
fail loudly.

Every rule is portable. A two-command CLI built on `clap` derive (or any argument parser)
can meet all of them without Orbit's operation registry, sink, or payload types. Orbit
appears only as the worked example. Where Orbit uses a heavy mechanism, the rule says what
the minimum compliant version looks like.

Rule numbers are stable. Rules added in v2 take the next free numbers (R26 onward) and sit
in the cluster they belong to, so numbers inside a cluster are not always in order.

This standard is a directory. This file holds the binding text: the rules, where they
apply, and how to deviate. Beside it, [`why.md`](why.md) gives the reason for each rule,
[`checks.md`](checks.md) the gate that enforces it, [`exemplars.md`](exemplars.md) worked
examples in Orbit, and [`CHANGELOG.md`](CHANGELOG.md) what changed in each version.

## Rules

### Command grammar

- **R1.** Commands MUST follow `<tool> <noun> <verb> [args] [flags]`, where a noun (the
  resource) groups its verbs as subcommands. A CLI with a single resource MAY drop the noun
  level (`<tool> <verb>`), but one tool MUST NOT mix the two orders.
- **R2.** A verb MUST mean the same thing on every noun, and verbs MUST come from one small
  shared vocabulary (`add`, `list`, `show`, `update`, `remove`/`archive`, …). `list`
  returns many records filtered by flags; `show` returns one record by its identifier.
- **R3.** Flags MUST be long-form kebab-case, and one concept MUST have one spelling
  across the whole command tree (`--workspace`, `--limit`, `--status`). A repeatable flag
  uses the singular form and repeats (`--tag a --tag b`). Short aliases are reserved for
  `-h`/`-V` and a few high-frequency flags. The one sanctioned second spelling is `--json`
  for `--format json` (R7). A flag's name is public surface: renaming or removing it is a
  breaking change and follows R35.
- **R4.** Cross-cutting options (context selection, data-root override, output mode) MUST
  be declared once as global flags that are accepted both before and after the subcommand.
  A command MUST NOT redeclare a global flag's name with a different meaning.
- **R26.** A subcommand's argument MUST NOT share its parser-internal identifier with a
  global argument. *clap note:* clap derive takes an argument's id from the field name and
  merges arguments that share an id, so a subcommand positional named `workspace` silently
  becomes the global `--workspace`. It parses, and its value goes to the global. Give such
  a field an explicit distinct id (`#[arg(id = "workspace_selector", value_name =
  "WORKSPACE")]`). A CLI with no global arguments, or a parser that keeps the two
  namespaces apart, complies trivially.
- **R5.** A destructive or irreversible command MUST NOT prompt or read stdin. It MUST
  refuse before changing anything unless `--confirm` is passed. A bulk or cleanup command
  defaults to a report (dry run) and applies only with `--confirm`. Add no other
  confirmation spelling.
- **R27.** An interactive prompt (allowed only for non-destructive commands, R5) SHOULD
  treat EOF or closed stdin as an error, never as an empty answer. It fails once, without
  re-prompting, with a message naming the flags that answer the prompt non-interactively
  (R21). An unanswered prompt on a non-terminal stdin SHOULD be bounded by a timeout
  rather than waiting forever.

### Inputs and effects

- **R28.** Every flag, selector, or override a command accepts, whether as a flag or as
  an environment variable, MUST take effect on every code path the command runs, or the
  command MUST reject it before doing any work. An invocation scoped by a root or context
  override (`--root`, `--workspace`, `TOOL_ROOT`) MUST NOT read or write state outside
  that scope, such as the user's home-level config, global skill links, or installed
  binaries. A command that cannot honor an override refuses it. It does not run unscoped.
- **R29.** A command or write surface MUST apply every field and argument it accepts, or
  reject the whole request and name each offending field. It MUST NOT silently drop,
  clamp, demote, or reinterpret caller input while reporting success. An unknown field is
  an error, not something to ignore. A partial update changes only the fields the caller
  supplied and leaves every other field as it was.
- **R30.** A command MUST NOT report success unless the effect it was asked for actually
  happened. A command that writes something MUST name the resolved target it wrote (the
  file, checkout, workspace, or record) in its human output and include it in its machine
  payload. If the request resolved to nothing (an excluded item, an inactive target), the
  command fails or says so. It does not print a bare "done".
- **R31.** A command that only reports (`list`, `show`, `status`, a check without `--fix`,
  a scan) MUST NOT create files or directories, take write locks, run migrations,
  reconcile or finalize records, or otherwise change durable state. It MUST work against a
  read-only data directory. Setup a read needs, such as first-run bootstrap or a schema
  upgrade, belongs to an explicit mutating command. The read fails with an actionable
  error (R21) if that setup has not been done.
- **R32.** Every identifier or selector the tool prints, in any output mode or on any
  surface (CLI, JSON, MCP, API), MUST be accepted back as input by every command and
  surface that takes that kind of identifier. A key a `show` lists MUST be settable or
  gettable by the matching `set`/`get`, or be labelled as derived and read-only.
- **R33.** A `list` MUST NOT apply a filter the caller did not ask for. Default filters
  (for example, hiding closed records) are either absent or stated in the command's help
  and echoed in the stderr notice. Filters MUST be applied before the limit, so `--limit N`
  returns the first N matching records, not the matches among the first N records.

### Output modes and streams

- **R6.** A command that returns data MUST build one structured payload and derive every
  rendering from it, human and machine. The human view may drop or reformat fields. It
  MUST NOT show a value the payload lacks or leave out a record the payload includes.
- **R7.** Every data-returning command MUST offer a machine mode, at minimum `--json`. That
  mode emits one JSON document: an array for a list (or the R34 envelope for a capped
  list), an object for a single record. A streaming command SHOULD also offer NDJSON: one
  complete JSON value per line, flushed as each record is written. Where the tool also has
  a `--format <mode>` flag, `--json` is the one permitted shorthand for `--format json`.
  Passing both with different modes MUST be a usage error (R20), not a silent precedence
  pick. Check the combination once after parsing, in the mode resolver (R8), because
  clap's `conflicts_with` does not reliably see a global argument that is given at a
  different subcommand level.
- **R8.** The output mode MUST be resolved once per invocation, in one place, with this
  precedence: explicit flag > environment variable > `auto`. `auto` renders for a human on
  a terminal and renders the piped form (R9) otherwise. An unrecognized environment value
  falls back to `auto` rather than failing the command.
- **R9.** When stdout is not a terminal, output MUST carry no ANSI escapes, no box-drawing
  or other decoration, and no width-based truncation. The default piped form SHOULD be one
  tab-separated line per record with no header, so `cut -f`, `grep`, and `wc -l` work
  without flags.
- **R10.** Machine output is a public contract. Field names MUST be stable `snake_case`.
  Renaming, removing, or retyping a field is a breaking change. So is changing the
  document's top-level shape, for example a bare array becoming an object envelope. A
  breaking change to machine output is on the same footing as renaming a flag (R35).
- **R11.** Machine values MUST be typed by meaning:
  - an absent value is `null`, never omitted;
  - timestamps are RFC 3339 with an explicit offset;
  - durations are integers with the unit in the field name (`duration_ms`);
  - counts are numbers;
  - enum-like values are their canonical lowercase token.

  Display formatting and casing belong to the human renderer.
- **R12.** stdout MUST carry only the payload. Progress, warnings, counts, pagination
  hints, empty-state prose, and diagnostics go to stderr in every mode. Progress
  indicators appear only when attached to a terminal, and never in a machine mode.
- **R13.** A closed stdout (`EPIPE`, as in `tool list | head -1`) MUST end the process
  silently with exit code `0`: no panic and no error message.
- **R34.** A list that returns fewer records than matched, because of a default or
  explicit limit, MUST say so explicitly in its machine payload: an object envelope
  carrying the records plus `total` (the number of matches, or `null` if unknown) and
  `truncated` (a boolean), for example `{"tasks": [...], "total": 133, "truncated": true}`.
  Choose that shape when the command is first designed, because switching a shipped bare
  array to an envelope later is a breaking change (R10). An NDJSON stream has no envelope,
  so its truncation signal is the stderr notice R12 already requires in every mode. A
  list with no limit returns every match and complies with a bare array.

### Tables

- **R14.** Human tables MUST be borderless, with one header row and exactly one line per
  record. Cells never wrap. Columns are separated by two-space gutters. Numbers are
  right-aligned, with the unit in the header (`DURATION (ms)`) rather than in every cell.
  An absent cell renders as `-`, never as blank. The number of body lines equals the
  number of records.
- **R15.** A value cut to fit its column MUST end in a single `…`. Truncation MUST apply
  only to the human terminal table, never to piped or machine output. Every column that
  can be truncated MUST be retrievable in full, through a detail command (`show`) or the
  machine mode. The width to fit comes from the one resolver in R17. When no width is
  known, the table MUST NOT truncate at all. It never assumes a default such as 80
  columns.
- **R16.** A query that matches nothing MUST exit `0` and print one line to stderr naming
  what was searched. In the human and piped forms, stdout is empty. In `--json` the one
  document is still emitted: `[]` for a list, or the R34 envelope with an empty record
  array. In NDJSON nothing is written, because zero records is an empty stream. A list
  command's shape MUST NOT depend on how many records match: a list of one is still a
  table (and still a JSON array), not a key-value view. This rule governs list-shaped
  output. A `show` detail command (R2) is not a list of one, and its human view MAY be a
  key-value layout.

### Color and TTY

- **R17.** Whether to emit color MUST be decided in exactly one place. Color is off when
  stdout is not a terminal, when `TERM=dumb`, or when `NO_COLOR` is set to a non-empty
  value. A non-empty `CLICOLOR_FORCE` turns it on for a terminal. The terminal width used
  for truncation (R15) is resolved in the same place. No command body may query TTY state,
  terminal width, or these variables itself. Minimum compliant form for a clap CLI:
  - build clap without its `color` feature (on by default) and without `wrap_help` (opt-in),
    because both make clap query the terminal on its own, and leaving them out also keeps
    help goldens (R24) identical across machines;
  - take the width from `COLUMNS` alone, or from `COLUMNS` with a terminal query
    (`TIOCGWINSZ`, or the `terminal_size` crate) as the fallback, and treat "no width"
    as "do not truncate". Most shells do not export `COLUMNS`, so a `COLUMNS`-only CLI
    will usually not truncate, which is compliant.
- **R18.** Color MUST be semantic and MUST NOT be the only way meaning is carried:
  - a small closed set of roles (ok, warn, error, active, muted, neutral) is mapped from
    domain values in one table;
  - an unmapped value renders as neutral;
  - roles apply to cells, not whole rows;
  - only the basic 16-color palette is used.

  With every escape stripped, the output loses emphasis but no information.

### Errors and exit codes

- **R19.** Errors MUST go to stderr in every mode. In a machine mode an error is one JSON
  object with at least `error` (the message) and a stable `snake_case` `code`. Otherwise,
  the first line is `error: <message>`. Further lines MAY follow it for usage or a tip, as
  clap prints them. A usage error that the parser raises before the output mode can be
  resolved MAY use that plain form in every mode, provided it exits `2` (R20). A CLI that
  wants JSON usage errors pre-scans the raw arguments (and the environment rung of R8)
  for the mode before parsing.
- **R20.** Exit codes MUST be `0` for success, `1` for a command failure, and `2` for a
  usage error (bad flag, unknown subcommand, or invalid value). A command that reported an
  error MUST NOT exit `0`. Any further code MUST be documented in that command's help.
- **R21.** An error message MUST be actionable. It names the input that was rejected and,
  where the fix is known, states it: `pass --confirm to proceed`, a did-you-mean
  suggestion, or the list of valid values.

### Help and public-surface stability

- **R22.** Every command and every flag MUST have help text that says what it does. A
  flag's help states its default and its allowed values. A command whose use is not
  obvious SHOULD end its help with an `Examples:` section.
- **R23.** User-facing text MUST NOT contain internal tracker identifiers (task, issue,
  friction, or decision-record numbers) or real record IDs. User-facing text means help,
  examples, error messages, output prose, and tool or MCP descriptions. Use placeholders
  such as `<id>` or `FYYYY-MM-NNN`. Code comments, commit messages, and design docs may
  cite tracker IDs.
- **R24.** The public surface MUST be golden-tested. The rendered `--help` of every command
  and the machine output of representative commands are checked-in fixtures, compared byte
  for byte in CI. They are regenerated only through an explicit update switch, and the
  diff is reviewed in the PR. Fixtures MUST be captured from the surface as it ships: the
  built binary, or the fully assembled parser including anything grafted on at startup.
- **R25.** Each command, flag, and help string MUST be declared in exactly one place. The
  parser, `--help`, and every other surface (JSON schema, MCP or tool definition, generated
  docs) are derived from that declaration. No flag name or help string is copied by hand
  into a second place.
- **R35.** Command names, flag names, accepted values, and config keys are public surface.
  Renaming or removing one, or changing what it means, is a breaking change. A removed or
  renamed flag or config key MUST stay accepted for at least one release, either as an
  alias of its replacement or as an ignored no-op. Each use prints a stderr warning that
  names it and its replacement, if there is one. After that release, it is rejected as a
  usage error (exit `2`) or a config load error naming the key. It is never silently
  ignored. The removal is recorded in the changelog.
- **R36.** Help, schemas, tool definitions, and docs MUST advertise only commands,
  parameters, and values that a runtime path actually implements. A parameter that is
  advertised but not wired through to the code that should act on it is a defect, not a
  placeholder. Where a surface is derived from a declaration (R25), a test proves that
  each advertised operation, and each parameter, reaches its implementation. Minimum
  compliant form for a small CLI: clap derive, with every field read by the command body
  (an unused field shows up in review, or as dead code), and no hand-written help that
  names flags the parser lacks.

## Applies to

- `cli` — R1–R36 (every rule; R26 is phrased for clap and holds trivially for a parser
  without global arguments)
- `service` — R29, R30, R32, R36 (any write surface, id-emitting surface, or advertised
  schema, such as an HTTP API or an MCP tool, not only a CLI)

## Deviations

An adopting repo that departs from a rule records a decision in its own design docs,
citing `STD-01@<version> §Rn` and the reason (for example `STD-01@2 §R4`). It never edits
its vendored copy of this standard.

Orbit carries these recorded deviations. Adopters should not copy them:

- `audit export --format` reuses the global flag's name with a different meaning (against
  R4).
- The legacy `--json` flag pretty-prints on every sink, while `--format json` pretty-prints
  only on a terminal. Orbit keeps this for byte-compatibility with older scripts.
- `--format <mode> --json` is not refused. `--format` silently outranks the legacy `--json`
  (against R7's conflict clause).
- `orbit task list --json` stays a bare array, because R10 froze its shape. Its truncation
  signal is only the stderr notice. The `orbit.task.list` tool and the dashboard API carry
  the `{tasks, total, truncated}` envelope (against R34).
- `orbit run history|show|logs|events` reconcile stale runs as a side effect of reading
  them. Observation-only reads need the opt-in `--no-reconcile` (against R31).
- Orbit builds clap with its default `color` feature, so clap colors its own help and
  usage errors outside the one gate. clap's detection follows the same `NO_COLOR` and TTY
  conventions, so the two agree in practice (against R17's minimum form).
