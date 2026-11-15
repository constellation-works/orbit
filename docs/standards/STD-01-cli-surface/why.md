# Why — STD-01 CLI surface

Why each rule in [STD-01](STD-01.md) exists, tied to the incident behind it where there
was one. Read the entry for a rule when its intent is unclear or before deviating. The
rules themselves are in `STD-01.md`; this file explains them and adds none.

- **R1–R2 (grammar).** Noun-verb grammar makes the command tree predictable: once a user
  knows `task list`, they can guess `friction list` and `task show`. A shared verb
  vocabulary means agents can guess commands correctly instead of reading help for each
  noun. The `list`/`show` pairing is also what makes truncation safe (R15).
- **R3 (flag spelling).** Every alternate spelling is something a user has to remember and
  an agent can get wrong. Colliding long flags only fail when clap builds the command, so
  Orbit asserts the whole tree once in a test (ORB-11765) instead of discovering the
  collision in an unrelated parser. `--json` is sanctioned as the one shorthand because it
  predates `--format` in most CLIs, and scripts already depend on it.
- **R4 (global flags).** Context and output mode apply to every command. Declaring them per
  command is how 86 of 150 Orbit argument structs came to carry their own independent
  `--json` before the global `--format` existed. Orbit's `audit export --format` names an
  export file type, not an output mode. It is a grandfathered exception and needed special
  handling in `install_format_arg` to avoid a downcast panic, which shows why R4 forbids
  reusing a name.
- **R26 (argument ids).** clap merges a subcommand argument into a global one when they
  share an id, and nothing warns. Orbit's `workspace remove <WORKSPACE>` positional was
  captured by the global `--workspace` selector. The documented recovery for a deleted
  checkout was therefore refused for every selector form, and the remove code was
  unreachable (ORB-12169, F2026-09-133). A debugging session lost about 25 minutes in files
  that were not involved. `task export/import/reindex` each declared a `--workspace` that
  the global flag consumed, so their documented task-registry ids were always rejected
  (ORB-12134). clap's `debug_assert` catches colliding long names but not this merge,
  which is why the rule has to name it.
- **R5 (confirmation).** A prompt hangs an agent or a CI job, and a stdin read in a pipe
  consumes the pipe's data. A consistent `--confirm` is safe for scripts and easy to
  discover. Orbit's design pattern records that a second spelling (`--yes`) exists only as
  a compatibility alias.
- **R27 (EOF at a prompt).** A prompt loop that reads EOF as an empty answer re-prompts
  forever: `orbit init` spun without end when stdin was closed (ORB-11595). Orbit's
  destructive-command pattern (`docs/design-patterns/command.md:19-27`) removes prompts
  where it matters most. R27 covers the prompts that remain. It is a SHOULD because a CLI
  with no prompts has nothing to do.
- **R28 (honor every override).** An override that is accepted but ignored is worse than
  one that is rejected. The caller believes they are scoped, so they carry on. `orbit init
  --root <custom>` re-linked the operator's global skill directories (ORB-10563, an
  incident). F2026-07-138 asks for `ORBIT_ROOT` to be a hard boundary on what init may
  touch. The same class recurred across ORB-10731, ORB-11165, ORB-11388, ORB-10928,
  ORB-11417, ORB-12129, ORB-12208 and ORB-13005. It also covers selectors that were
  accepted and then ignored (ORB-13022), `mcp init/remove` bypassing the registry
  (ORB-12121), and `update --root` still replacing the host's binary (ORB-12586). The
  precedent for refusing is `orbit mcp serve`, which rejects `--root` outright instead of
  half-honoring it.
- **R29 (no silent coercion).** A write that drops a field it accepted tells the caller
  their intent was recorded when it was not. Examples: retired `task.add` fields were
  stripped silently (ORB-11698), `--fields` returned `{}` (ORB-12205), and an operator's
  `xhard` complexity was quietly demoted (ORB-12622). ORB-11666 is another case.
  F2026-07-110 states the rule: "An unknown argument to a write surface should be an
  error, not a shrug." The incident form is F2026-07-056: a PR-gated repository silently
  fell back to local shipment. F2026-07-080, F2026-08-070 and F2026-08-048 are the same
  class. Orbit's dashboard settled on all-or-nothing rejection of unsupported body fields
  (`codebases/orbit/docs/design/user-interface/4_decisions.md:231`). Rendering unknown or
  failed values honestly, rather than as zero or success, is STD-02's side of the same
  principle.
- **R30 (success means the effect happened).** `tool enable` reported success on tools
  whose availability is fixed at registration, so nothing changed (ORB-12122). `run show`
  stayed silent about tasks that were explicitly shipped but excluded (ORB-12133). Setup
  wrote config paths the client never reads (ORB-12149), and ORB-12223 is another case.
  `mcp init/remove` never named the checkout they wrote, so a wrong resolution was
  invisible (ORB-12132). Naming the resolved target turns a mis-scoped write into
  something a human or agent can see immediately.
- **R31 (reads never write).** A read that writes breaks read-only sandboxes, races real
  writers, and changes state as a side effect of looking at it. Every `orbit` command
  once rewrote the global managed-asset manifest unconditionally (ORB-10713, fixed by
  ORB-10732's no-op-when-unchanged rule). Run-failure scans finalized orphaned runs and
  released their reservations just by reading them (ORB-12941, ORB-12943). A read failed
  on a read-only path inside an executor sandbox (F2026-08-060), and a run failed before
  implementation for the same reason (F2026-09-143).
- **R32 (identifiers round-trip).** An id the tool prints but will not accept back forces
  the caller to translate between forms, and an agent will not know that it needs to.
  The host-qualified selectors that MCP hands out were rejected by `tool run` (ORB-13021).
  `config show` listed a key that `config get`/`set` refuse (ORB-12339). A task-registry
  id printed by `task import` was refused by every `--workspace` (ORB-12134).
- **R33 (no hidden filters, filter before limit).** A hidden default filter makes a
  record look missing when it is not. Orbit's `task list` used to show only
  `backlog,in-progress` by default, until ORB-10310 made it status-neutral, recent-first
  and bounded, with filters applied before the limit. F2026-07-111 is the other half: a
  state filter applied client-side over an already-limited window under-reported running
  runs.
- **R6 (one payload).** When the JSON and human views are built separately in the same
  function, they drift. In Orbit, `tool list` emitted seven JSON fields but five table
  columns. ORB-10228 added provenance fields to the audit JSON and left the printed line
  untouched. Deriving the human view from the payload makes this kind of disagreement
  impossible by construction. A small CLI can comply with a `struct` that derives
  `Serialize` plus a `fn render_human(&T)`; Orbit's `Payload`/`CommandOutput` types are one
  implementation.
- **R7–R8 (modes).** Scripts and agents need a flag that always yields parseable output.
  Resolving the mode centrally means precedence is defined once instead of per command.
  The environment rung (`ORBIT_FORMAT`) lets a whole session opt in, and falling back on a
  bad value keeps one exported typo from breaking every command in that shell. Refusing a
  `--json --format table` conflict follows R28: one of two explicit inputs would
  otherwise be silently ignored. The "check after parse" note comes from building the Rust
  CLI template against v1 (ORB-13117).
- **R9 (piped form).** Before ORB-10567 and ORB-10570, a piped `orbit tool list` received
  box glyphs, wrapped multi-line rows, and ANSI escapes, so `grep` returned fragments.
  Orbit's headerless tab-separated plain form comes from `gh`'s behaviour off a TTY. It
  also never suppresses uniform columns: ORB-12113 found that dropping one silently shifted
  every later `cut -f` field.
- **R10–R11 (payload contract).** Consumers index machine output by key. A renamed field
  breaks them silently, which is worse than a renamed flag, because a flag at least errors.
  A shape change breaks them the same way. `orbit task list --json` was silently changed
  from a bare array to a `{tasks, total, truncated}` envelope, which broke the frozen
  per-command contract, and ORB-12198 reverted it. `null` instead of omission means "not
  applicable" and "missing" read the same way. Typed values keep display formatting out
  of the contract.
- **R12 (stdout is the payload).** Prose mixed into stdout turns into a phantom record for
  every consumer. Orbit prints hints such as `showing 50 of 133 tasks …` on stderr, so
  `--json | jq` and `| wc -l` stay correct. ORB-10570 moved the JSON error object from
  stdout to stderr for the same reason. That move was a deliberate breaking change,
  announced in the changelog. Human rows printed before JSON (ORB-10791) and unparseable
  `doctor --fix-* --format json` stdout (ORB-11597, ORB-11596) are the same defect.
- **R13 (EPIPE).** `tool list | head` is normal use. Rust's `println!` panics on a closed
  pipe, and the resulting panic text looks like a crash to the user.
- **R34 (explicit truncation).** A silently capped page looks exactly like a complete one.
  Orbit shipped silent truncation twice. The MCP `task.list` returned a bare array capped
  at 50 while the CLI had gained a notice (ORB-12195). Then the ORB-12198 revert removed
  the only machine truncation signal and left `--json` callers with a capped page and no
  notice (ORB-12203). The terminal-interface decisions state the principle: "The payload
  is the contract" (`codebases/orbit/docs/design/terminal-interface/4_decisions.md:44`,
  `:57`). Putting `total`/`truncated` in the payload, in a shape chosen up front, avoids
  both failures.
- **R14–R16 (tables).** The borders existed only to separate wrapped rows (ORB-10567). Once
  a row is a single line, a border is noise to `grep` and `awk`. Cutting a value with no
  `…` and no way to recover it causes silent data loss, which is why truncation always
  comes with a detail command. Guessing a width for a sink that has none truncates output
  that nothing will display at that width. Keeping empty-state prose off stdout means a
  pipe receives an empty stream, not a sentence. `[]` in `--json` keeps "one document"
  (R7) true for zero records. A shape that stays the same regardless of record count means
  a consumer never needs to branch on the count. The `show` carve-out resolves v1's
  ambiguity with R2 and R15, which define `show` as the one-record detail command. That
  ambiguity was raised by the constellation-standards skill author.
- **R17 (one color gate).** Orbit's two styling crates each ran their own `NO_COLOR` and
  TTY checks, and they disagreed: `comfy_table` ignored `NO_COLOR` and wrote escapes into
  redirected files (ORB-10570). One decision point, which overrides every backend, is the
  only arrangement that stays correct as more backends are added. The `NO_COLOR`
  convention (no-color.org) is the baseline users expect. clap's `color` and `wrap_help`
  features are exactly such a second backend, which is why the minimum form leaves them
  out. That lesson comes from the template build (ORB-13117).
- **R18 (semantic color).** Orbit's status palette was defined twice, and the two copies
  drifted: `backlog` was colored in one and not the other (T20260427-43, ORB-10202). A
  closed role set bounds the palette, so it can be audited for contrast. Because the word
  always prints, `NO_COLOR` and piped output are correct, not degraded.
- **R19–R20 (errors).** Scripts should branch on the exit code and the stable `code` value,
  never on message wording. Following clap's convention of exit code `2` for usage errors
  lets a wrapper tell "you called it wrong" from "it ran and failed". Allowing clap's
  multi-line usage error, and a plain usage error when the mode cannot yet be known,
  matches what a parser can actually do before it has succeeded (ORB-13117). The exit
  code still carries the machine-readable fact. Error typing and the mapping to codes at
  crate boundaries belong to STD-02.
- **R21 (actionable errors).** Every error message that names the fix saves a round trip
  through `--help`, for a human and, more expensively, for an agent.
- **R22 (help).** For agents, help is the primary way to discover how a command works.
  Examples show real invocation shapes faster than a list of flags.
- **R23 (no internal IDs).** A tracker ID in help or output means nothing to an outside
  user and goes stale when the tracker moves. Real record IDs in examples leak one
  workspace's data into every other workspace. This is a standing rule in Orbit's
  `AGENTS.md`, and Orbit enforces it with a recursive scan of the help tree.
- **R24 (goldens).** Help and machine output are contracts, and in Orbit they drift through
  incidental causes: a clap upgrade, reordered fields, a new global flag. A byte-for-byte
  fixture turns "wire compatible" from a claim in review into a check (ORB-10358 froze the
  friction help before migrating it; ORB-10571 added the output goldens). Capturing from
  the shipped surface matters: Orbit's help goldens render from the derive tree, before
  `main` grafts on `--format`, so they do not match what users see (see [`exemplars.md`](exemplars.md)).
- **R25 (declare once).** Orbit's friction noun was once written out on four surfaces: CLI,
  MCP, dashboard, and runtime. `show` and `update` described the same `id` field in two
  different ways (ORB-10358). For a small CLI, clap derive already satisfies R25, since the
  struct is the single declaration and help comes from its doc comments. Orbit's
  `OperationSpec` registry is what R25 looks like once several surfaces (CLI and MCP) have
  to agree.
- **R35 (deprecation window).** A flag or key that vanishes between releases breaks every
  script and config file that used it, all at once. One that is silently ignored forever
  hides that the caller's intent no longer does anything. Orbit settled on the same
  migration three times: the routines `hosts:` key is "accepted and ignored with a load
  warning for one release, then rejected"
  (`codebases/orbit/docs/design/routines/4_decisions.md:250`). The retired
  `owner_host_ids` catalog key was dropped for one release instead of failing every
  command (`…/host-registry/4_decisions.md:201`). Removed operation-mode keys are warned
  about by name and ignored at load (`…/orbit-core/4_decisions.md:191`). v1 said a flag
  rename was breaking only by analogy to a field rename (R10). The skill author asked for
  it to be explicit.
- **R36 (advertise only what runs).** An advertised capability that does nothing is a
  trap for agents, who discover commands through help and schemas. A shipped asset
  described executor behaviour that no runtime implemented
  (`codebases/orbit/docs/design/executors/4_decisions.md:69`). An operator-only tool
  shipped without its CLI subcommand is "an unreachable surface, which is what happened
  here" (`…/orbit-core/4_decisions.md:147`). A feature shipped with no reachable caller
  (F2026-08-005). An advertised `state` filter never reached the backend, and no test
  checked that it did (F2026-07-111). ORB-12581 is another case.
