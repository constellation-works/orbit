---
type: context
summary: Orbit Configuration
last_validated: 2026-09-07
---

# Orbit Configuration

Reference for Orbit's runtime config — the `config.toml` consumed by `orbit run ship` and the activity-job dispatcher. The defaults shipped with the binary live in [`crates/orbit-config/assets/default-config.toml`](../crates/orbit-config/assets/default-config.toml).

This doc focuses on the user-facing knobs: `[workflow]` and `[crews.*]`. Other sections are summarized at the end.

Contributors adding a new execution lane or deterministic command executor should
use the [executor onboarding runbook](runbooks/executor-onboarding.md). It
documents the v2 seams and validation obligations; this reference remains the
operator contract for configuring an already shipped lane.

## Where config lives

Two paths are consulted, in order:

| Path | Scope | Created by |
|---|---|---|
| `<workspace>/.orbit/config.toml` | Workspace-local | Hand-authored (optional) |
| `~/.orbit/config.toml` | Global / user | `orbit init` |

Ordinary settings inherit per key: workspace values override global values, global values fill omissions, and built-in defaults fill remaining gaps.

Tables layer down to individual settings, while scalar and array values replace the matching global value. Named crews layer by crew name and field, so this is a complete workspace override when the global file already defines `sol`:

```toml
[crews.sol]
model = "gpt-5.6-terra"
```

Three security-sensitive settings deliberately do not inherit from global whenever a distinct workspace file exists:

- `execution.codex.sandbox`
- `execution.codex.approval_policy`
- `execution.env.pass`

If the workspace file omits one of these, Orbit uses that setting's built-in default. This keeps repository agent sandboxing, approval, and environment passthrough deterministic instead of depending on a user's global policy. `execution.env.inherit` is not a configurable key: an agent subprocess environment is always composed from an allowlist — see [`[execution.env]` — the agent subprocess environment](#executionenv--the-agent-subprocess-environment).

Run `orbit config show` for the effective merged view. Every setting is annotated as `workspace`, `global`, `built-in`, or `environment`, including the source file path where one applies. `orbit config show --json` exposes the same attribution in its `provenance` object. Use `--scope global` or `--scope workspace` to inspect either physical file alone.

The workspace identity file `.orbit/config.yaml` is a separate artifact (it stores `workspace_id` for the canonical task store binding) and is unrelated to runtime config.

---

## `[workflow]` — branch and crew defaults

```toml
[workflow]
base_branch = "main"        # default merge-base for ship
default_crew = "sol"        # fallback crew when a task has no `crew` set
system_crew = "system"      # crew for recovery paths with no job step to name one
```

- **`base_branch`** — the branch `orbit run ship` rebases against and targets with PRs. Override per-invocation with `--base <branch>`. If your repo uses a two-branch pattern like this repo does (`main` = release, `agent-main` = dev integration), set `base_branch = "agent-main"`.
- **`default_crew`** — name of the crew under `[crews.<name>]` used for any task whose own `crew` field is unset. Must match a defined crew or config load fails. See [Per-task crew override](#per-task-crew-override) for how individual tasks select a different crew.
- **`system_crew`** — name of the crew for system activities that are synthesized at runtime and so have no job step to name a crew on, principally `step_failure_recovery`. Defaults to `system`. Shipped pipelines such as `task_pilot_pipeline` and `task_triage_pipeline` do **not** read this key: their steps name `crew: system` directly, so the definition states which crew does the work. Either way the crew is resolved at dispatch through an explicit crew input, so system work never inherits a failed task's crew or the workspace default. A missing or unusable crew leaves the original failed step failed and emits a diagnostic naming `workflow.system_crew` and the configured crew.

  **The `system` crew.** Interactive `orbit init` (or `--force` on a fresh rewrite) asks which detected bounded family should back `[crews.system]`: Codex Luna (`gpt-5.6-luna`), Claude Sonnet, Grok (`grok-4.6`), Antigravity Flash (`gemini-3.8-flash-low` via `agy`), Gemini Flash (`gemini-3.8-flash` on the legacy Gemini CLI), Copilot Haiku, Cursor (`gpt-5`; Cursor has no stable cheap-tier alias), or Pi (`sonnet`; Pi has no stable cheap-tier alias either). It does not offer Astra, Sol, Opus, Terra, or a free-form custom provider, and it never prompts for a QA crew. `workflow.system_crew` stays `system`; only the assignment behind that name is chosen. A host with exactly one of those families auto-accepts it; a host with none omits `[crews.system]` rather than inventing a provider. `--non-interactive` never prompts and still auto-seeds `[crews.system]` from the preference order: Codex Luna, then Claude Sonnet, then Grok, then Antigravity Flash, then Gemini Flash, then Copilot, then Cursor, then Pi. Appending the newer lanes preserves every existing family's selection. To change what runs system work after init, edit `[crews.system]`. Configs written before this crew existed have no such table, so the name is resolved first onto the crew `system_crew` names when that crew exists. For Orbit's default or legacy lane names (`system` and `qa`), a missing crew falls back to an existing `qa` crew and then to the already-validated workspace default; the latter keeps old Gemini- and Grok-only configs working even though they never seeded `qa`. Unknown custom names are not substituted. A host that points `system_crew` at a defined cheap crew therefore keeps running system work there rather than being silently relocated. An explicit `[crews.system]` always wins. `[crews.qa]` remains a loadable compatibility lane for explicitly user-authored legacy configs, but fresh init never creates it.

---

## `[crews.<name>]` — which provider-model runs the task

A **crew** is one provider-model assignment. Activities do not carry a model-selection role: a rendered activity input may name a `crew`, and otherwise the activity inherits the run's resolved crew.

| Field | Purpose | Values |
|---|---|---|
| `model` | Model identifier passed to the provider CLI | Provider-specific (e.g. `opus`, `sonnet`, `gpt-6-astra`, `gemini-3.8-flash-high`, `grok-4.6`) |
| `provider` | Agent family or execution lane | `claude`, `codex`, `antigravity`, `gemini`, `grok`, `copilot`, `cursor`, `pi`, `opencode` (the CLI-executable crew families; see [Provider identity and resolution](#provider-identity-and-resolution) for the full canonical set) |
| `effort` | Optional provider reasoning effort | Claude/Codex: `low`, `medium`, `high`, `xhigh`, or `max`; Antigravity: `low`, `medium`, `high`; OpenCode: `high` or `max`; Grok: verified per model below |
| `description` | Optional human-facing crew summary | Any non-empty string after trimming |
| `tags` | Optional discovery labels | Array of strings; normalized, sorted, and deduplicated |

Example — the standard Codex Sol crew:

```toml
[crews.sol]
model = "gpt-5.6-sol"
provider = "codex"
effort = "high"
description = "Systems implementation"
tags = ["implementation", "review"]
```

`effort` is omitted by default, which leaves the provider's existing model
default unchanged. When set, Orbit validates it while loading `config.toml`
and passes it through the provider's documented argv: Codex receives
`model_reasoning_effort`, Claude receives `--effort`, Antigravity receives
`--effort` (`low`/`medium`/`high` only), and Grok receives
`--reasoning-effort`. The installed Claude CLI (2.1.261, checked September
2026) advertises all five values, so Orbit forwards `low`, `medium`, `high`,
`xhigh`, and `max` exactly; [Claude's effort documentation](https://code.claude.com/docs/en/model-config)
notes that availability can still depend on the selected model. Grok Build
1.0.13 advertises `--reasoning-effort` (with `--effort` as an alias), and
`grok models` currently lists `grok-4.6` and `grok-4.5`. Orbit accepts `low`,
`medium`, `high`, and `xhigh` for `grok-4.6`, and `low`, `medium`, and `high`
for `grok-4.5`, matching [xAI's reasoning contract](https://docs.x.ai/developers/model-capabilities/text/reasoning).
It rejects `max`, unsupported Grok model/effort pairs, Antigravity `xhigh`/`max`,
OpenCode `low`/`medium`/`xhigh`,
legacy Gemini CLI model ids on the Antigravity lane, and effort on other
providers clearly rather than silently downgrading or ignoring a request. A
selected activity crew (including `workflow.system_crew`) supplies its effort
together with its provider and model, so it takes precedence over an activity's
inline baseline in the same way as the rest of that assignment. For the
standard Codex tiers, use the model-specific crew to choose capability first:
Terra (`gpt-5.6-terra`) is the medium-low crew; `effort` adjusts the reasoning
budget inside the chosen Codex model.

Named crew fields are addressable through `orbit config` as
`crews.<name>.<field>` (`model`, `provider`, `effort`, `description`, `tags`):

```bash
orbit config set crews.sol.effort high
orbit config get crews.sol.effort
orbit config show --json
```

`get` and `show` report the same configured effort the runtime assignment
uses. `show` includes `crews.sol.effort` with `workspace` or `global`
provenance when the field is set; omitting it leaves the provider default and
does not invent a configured value in effective output. Invalid values,
unsupported provider/model combinations, and misspelled crew fields are
refused before the file is written. Creating a crew still requires a
`[crews.<name>]` table with `model` and `provider` — `config set` will not
persist an incomplete crew.

Example — the standard Grok crew:

```toml
[crews.grok]
model = "grok-4.6"
provider = "grok"
```

The current Grok Build CLI lists `grok-4.6` as its default from `grok models`, so Orbit uses that live menu id. The older `grok-build` string is not retained as a default or alias.

Fresh `orbit init` configuration advertises only detected provider CLIs. Claude seeds `opus`, `sonnet`, and `fable`; Codex seeds `astra`, `sol`, `terra`, and `luna`; an installed `agy` seeds `antigravity`; Gemini CLI still seeds the legacy `gemini` crew when that binary is present; Grok seeds `grok`; Copilot seeds `copilot`; an installed `cursor-agent` seeds `cursor`; and an installed `pi` seeds `pi`. Antigravity occupies Gemini CLI's previous default-provider slot, so a host with both `agy` and `gemini` prefers Antigravity. Copilot, Cursor, and Pi remain appended after the original families. Interactive init still asks for the default crew (`[crews.custom]`) and, separately, for the system crew written as `[crews.system]`; it does not ask for QA. `--non-interactive` auto-seeds `[crews.system]` from the preference order above whenever a supported family is detected. The legacy `qa` name remains loadable when an existing user-authored config defines `[crews.qa]`, but init does not seed that table. If no supported provider CLI is detected, init leaves both the crew registry and `workflow.default_crew` unset instead of writing an unusable provider.

The `astra` crew (`gpt-6-astra`) is the Codex fresh-config default; the existing `sol`, `terra`, and `luna` crews remain available. The Antigravity `antigravity` crew uses `gemini-3.8-flash-high` from `agy models` (verified against Antigravity CLI 1.1.27). The legacy `gemini` crew still uses `gemini-3.8-flash` for enterprise Gemini CLI. These exact IDs are not remapped: a crew that names `gemini-3.8-flash` on `provider = "antigravity"` fails with migration guidance. Existing explicit model pins are retained when their configuration loads.

You can define any number of crews. Set the workspace-wide fallback with `workflow.default_crew`; assign a specific crew to individual tasks via the [per-task crew override](#per-task-crew-override). Crews are validated at load time: each crew must have non-empty `model` and `provider`; `workflow.default_crew` must name a defined crew.

Crew metadata is runtime data, not display-only TOML. Orbit trims `description`
(blank becomes absent), trims each tag, drops blank tags, and stores tags in sorted
deduplicated order. `orbit.crew.list` reads and normalizes the selected checkout's
effective local configuration on the machine serving the request. It returns the
versioned `CrewDiscoveryV1` projection directly; no execution-profile publication or
registry database is involved.

> **Retired crew shape.** `planner`, `implementer`, and `reviewer` sub-tables
> are no longer accepted in a crew entry. A workspace using that old shape must
> rewrite every `[crews.<name>]` entry to set flat `model` and `provider`
> fields before Orbit can load its configuration. Use separate crew-bound runs
> when comparing providers.

> **Retired `backend` field.** `[crews.<name>] backend` selected the agent
> execution backend. Orbit executes agent activities through the CLI agent path
> only, so the setting no longer chooses anything: `backend = "cli"` is accepted
> and ignored, while `"http"` and `"auto"` are rejected at config load with the
> migration message. Remove the key. Orbit never rewrites `http` to the CLI
> agent for you — that would change which runtime a crew dispatches to without
> saying so.

> **Note.** Earlier Orbit versions used `[agent.<role>]` tables. That schema was removed in [ORB-00058](../.orbit/) — config load now hard-errors if `[agent.*]` is present. Migrate to `[crews.<name>]` + `workflow.default_crew`.

---

## Provider identity and resolution

Every `provider` string Orbit reads — in `[crews.<name>]`, in an activity's inline `provider`, and in setup detection — is parsed through **one canonical surface** (`orbit_types::workflow::Provider`, ORB-10091). Centralizing parsing means the crew resolver, the CLI executor, and reconciliation cannot disagree with each other or with Worker/Bridge about what a provider name means.

### Canonical providers

| Canonical id | Aliases | CLI runtime | HTTP transport | Worker-executable |
|---|---|---|---|---|
| `claude` | — | yes | yes | yes |
| `codex` | — | yes | no | yes |
| `gemini` | — | yes | no | yes |
| `grok` | — | yes | no | yes |
| `copilot` | — | yes | no | **no** |
| `ollama` | — | **unsupported at the Orbit CLI entry point** | no | **no** |
| `openai_compat` | `openai-compat` | **no** (HTTP-only) | no | **no** |
| `cursor` | — | yes (`cursor-agent`) | no | **no** |
| `pi` | — | yes (`pi`) | no | **no** |
| `antigravity` | — | yes (`agy`) | no | **no** |
| `opencode` | — | yes (`opencode`) | no | **no** |

- **Parsing is case- and whitespace-insensitive.** `Claude`, `  claude `, and `CLAUDE` all resolve to `claude`. `openai-compat` normalizes to `openai_compat`.
- **Deprecated aliases resolve *and* warn.** The legacy vendor names normalize to their canonical id and log an `orbit.config.crew` deprecation warning (`{alias, canonical}`) — they never fail, but update the config:

  | Deprecated alias | Canonical |
  |---|---|
  | `anthropic` | `claude` |
  | `openai`, `chatgpt` | `codex` |
  | `google` | `gemini` |
  | `xai` | `grok` |

  `copilot`, `cursor`, `pi`, `antigravity`, and `opencode` have **no** aliases.
  `github`, `cursor-agent`, `anysphere`, `pi-coding-agent`, `earendil`, `agy`,
  and `sst` are not provider spellings, and the vendor that supplies a session's
  underlying model never changes its execution-lane identity. `google` still
  aliases to `gemini` (the model family / legacy Gemini CLI), not Antigravity.
  See [GitHub Copilot CLI](#github-copilot-cli),
  [Cursor Agent CLI](#cursor-agent-cli), [Pi CLI](#pi-cli),
  [Antigravity CLI](#antigravity-cli), and [OpenCode CLI](#opencode-cli).

- **Canonical ≠ Worker-executable.** Orbit's canonical set is deliberately wider than what the model-neutral Worker leaf executor can run: `copilot`, `cursor`, `pi`, `antigravity`, `opencode`, `ollama`, and `openai_compat` are first-class Orbit providers but Worker does not execute them. This distinction is preserved on purpose — do not narrow the canonical set to Worker's subset. For `copilot`, `cursor`, `pi`, `antigravity`, and `opencode` this is a *stable diagnostic*, not a fallback: a Worker-routed step naming one of those lanes is refused by identity rather than silently re-pointed at another family.
- **Known ≠ executable at this entry point.** The shared contract recognizes `ollama`, but the Orbit CLI capability set is the canonical cross-repo four; explicitly selecting `ollama` fails as `provider.unsupported` rather than falling back.
- **`openai_compat` has no CLI runtime.** Every crew dispatches through the CLI agent path, so selecting it fails structurally (see below) rather than falling back.

### Resolution precedence

Provider selection is **three composed steps**, not one. Describe them precisely — the inline `provider` on an activity is the *template baseline*, **not** an explicit override that outranks the crew.

**1 — Which crew is dispatched** (`resolve_crew_for_task`). The crew *name* is chosen by the Constellation provider-resolution precedence (contract §3), first non-empty tier wins:

1. **explicit** — `--crew` flag / run-input `crew`.
2. **task_config** — `task.crew` on the task artifact.
3. **workspace_default** — `[workflow].default_crew` in `config.toml`.
4. **environment_default** — the `CONSTELLATION_DEFAULT_PROVIDER` environment variable (a canonical provider id, which names the same-named single-family crew).
5. **system_default** — the canonical baseline (see below).

**2 — Which crew an activity uses.** A non-empty `crew` in the activity's rendered input selects that named crew. Without one, the activity uses the run's resolved crew from step 1. This is the only activity-authoring routing mechanism. Activity and job assets that declare `role` are rejected with guidance to pass `crew` in the activity input instead.

**3 — The activity crew's assignment overrides the inline baseline** (`resolve_from_config`). For each `(provider, model, effort)` field independently: the selected crew value wins **when present**; otherwise the activity's inline `agent_loop` value stands. A crew assignment that omits a field (or whose `provider` string is unparseable) leaves the inline baseline in place — so a config typo never coerces dispatch onto a wrong runtime. This is also why **persisted provider identity is never re-defaulted** during reconciliation: a provider already frozen on a run record is reused verbatim, not reset to the enum default.

### The one setting that changes the default — `CONSTELLATION_DEFAULT_PROVIDER`

`CONSTELLATION_DEFAULT_PROVIDER` occupies the **environment_default** tier (4) — below any explicit / task / workspace choice, above the persisted baseline. Setting it to a canonical id (or a deprecated alias, which normalizes) re-defaults **every otherwise-defaulted resolution path at once**, without editing any repo or per-workspace config; a path that already made a higher-precedence choice is deliberately unaffected. Because Orbit seeds `[workflow].default_crew` on `orbit init`, a normally-configured workspace resolves at the workspace tier, so the env lever governs paths that reach resolution without a configured crew and **never overrides a configured `[workflow].default_crew`**.

> **System default.** The canonical Constellation system default is `claude`. When no higher tier selects a crew, Orbit dispatches the same-named `claude` crew. Existing workspaces whose `[workflow].default_crew` is `codex` retain that higher-precedence configured choice; the system fallback does not rewrite workspace configuration.

### No silent fallback

Explicit selections that are unsupported or unavailable **fail with a stable diagnostic and never fall back** to a different runtime:

- `provider openai_compat is unsupported by the Orbit CLI entry point (HTTP-only)` — a CLI-executable dispatch selected an HTTP-only provider.
- `provider ollama is unsupported by the Orbit CLI entry point` — a known provider is outside this entry point's capability set.
- `unknown provider '<x>'; expected one of claude, codex, gemini, grok, copilot, ollama, openai_compat, cursor, pi, antigravity, opencode — no CLI runtime registered` — the provider string did not resolve to a canonical id.

An **unrecognized `[crews.<name>].provider` value** is the one non-fatal case: it is logged (`orbit.config.crew` warn) and that field falls back to the activity's inline `provider`, because a config typo should not coerce dispatch onto a wrong runtime — the inline value is the known-good identity, not a default guess.

---

## GitHub Copilot CLI

Orbit dispatches Copilot through the **standalone `copilot` CLI** (npm package
`@github/copilot`), which provides a non-interactive programmatic mode.

> **The retired `gh-copilot` extension is not supported.** `gh copilot` was a
> shell-command *suggester*, not an agent: it could not edit files or run a
> turn to completion, so it cannot satisfy Orbit's completion-envelope
> contract. Orbit never probes for it, never dispatches to it, and installing
> it does not make the `copilot` provider available.

### Installation

```sh
npm install -g @github/copilot
copilot --version
```

`orbit init` detects the `copilot` binary on `PATH` and offers the `copilot`
crew. Detection is by binary presence only — see
[Authentication](#authentication) for what a *working* run additionally needs.

### Organization-policy prerequisites

Copilot is organization-governed, and its policy checks happen server-side
after the CLI starts. Two failures are common and are **not** Orbit
misconfiguration:

- **No Copilot entitlement.** The CLI exits non-zero with
  `Error: Authentication failed` and advises checking the token's
  `Copilot Requests` permission. Orbit reports the step as failed; it never
  falls back to another provider.
- **Third-party MCP servers disabled by policy.** The CLI emits a
  `session.warning` frame with `warningType: "policy"` and continues with
  built-in servers only.

Both require a change by the GitHub organization administrator, not by Orbit.

### Authentication

Copilot resolves credentials in this documented order:

1. `COPILOT_GITHUB_TOKEN`
2. `GH_TOKEN`
3. `GITHUB_TOKEN`
4. Otherwise, the credentials stored by `copilot` itself via its `/login`
   command, under `COPILOT_HOME` (default `$HOME/.copilot`).

**Orbit does not forward those token variables on the provider's behalf.**
Agent subprocesses get an allowlist-composed environment, and credentials are
admitted only when an operator names them, so an unrelated `GITHUB_TOKEN` left
in the environment cannot be silently borrowed by a Copilot run. To use
token-based authentication, add the variable explicitly:

```toml
[execution.env]
pass = ["COPILOT_GITHUB_TOKEN"]
```

Token *values* are never logged, recorded in audit argv, or included in error
messages. The recommended setup is `copilot` `/login` once on the host, which
needs no token in the environment at all.

`COPILOT_HOME` is forwarded to the provider subprocess and is also what the
sandbox grants, so the directory the CLI writes to and the directory Orbit
allows cannot drift apart.

### Model selection

Orbit always passes `--model` explicitly, from the crew assignment. Without it
the CLI would fall back to `COPILOT_MODEL` or its own persisted `/model`
choice, which would make a run's model depend on ambient operator state rather
than on configuration.

```toml
[crews.copilot]
model = "claude-sonnet-4.5"
provider = "copilot"
```

Copilot routes to several vendors' models (`claude-*`, `gpt-*`, `gemini-*`).
**The provider identity stays `copilot` regardless.** A crew running
`gpt-5.4` through Copilot is a `copilot` run, not a `codex` run: the execution
lane, its authentication, its policy, and its sandbox grants are Copilot's.
Run `copilot --model <id>` or the interactive `/model` command to see the ids
your organization currently allows.

### Sandbox and permissions

Orbit's activity sandbox remains the security boundary. The shipped executor
passes `--allow-all-tools` so the agent does not block waiting for approval,
together with `--no-ask-user`. It deliberately does **not** pass `--allow-all`
or `--yolo`: those also imply `--allow-all-paths` and `--allow-all-urls`, which
would widen the agent's reach past what the enclosing Orbit sandbox granted.

When a Copilot executor is the active provider, the sandbox additionally grants
write access to `COPILOT_HOME` (default `$HOME/.copilot`) and to the launcher's
package-extraction cache (`$XDG_CACHE_HOME/copilot`, default
`$HOME/.cache/copilot`). Those grants are **gated on Copilot being the provider
actually running** — other providers do not inherit them.

Copilot is not granted read access to the GitHub CLI's credential store
(`~/.config/gh`) or to the macOS login keychain; it authenticates from its own
`COPILOT_HOME` or from an operator-passed token.

### Prompt transport

The Orbit execution envelope is written to the agent's **standard input**, not
passed as `-p <text>`. Both are supported by the CLI, but argv is visible in
process listings and is recorded in Orbit's audit argv, so the prompt — which
carries task context and instructions — must not travel there.

Copilot's stdout is JSONL agent events (`--output-format json`). Orbit reads
completion evidence only from model-authored frames; a run that emits no
assistant message has not completed its contract, and Orbit fails the step
rather than inferring success from the session control plane.

---

## Cursor Agent CLI

The `cursor` provider launches the local `cursor-agent` binary as an
Orbit-supervised worker. Cursor cloud agents are not used.

### Installation and detection

Install and verify the supported local CLI using Cursor's documented command:

```sh
curl https://cursor.com/install -fsS | bash
cursor-agent --version
```

Ensure the installed directory (normally `$HOME/.local/bin`) is on `PATH`
before running `orbit init`. Fresh init detects `cursor-agent`, adds a
`[crews.cursor]` assignment, and can choose it only after every previously
supported family in the preference order. Selecting `cursor` when its binary
is unavailable fails with a permanent diagnostic naming `cursor-agent`; Orbit
never falls back to Codex or another model vendor.

### Authentication and credential handling

Cursor supports two local CLI authentication paths:

1. Run `cursor-agent login` once and verify it with `cursor-agent status`. The
   login state is stored under `$HOME/.cursor`.
2. Generate a Cursor user API key and explicitly pass `CURSOR_API_KEY` to the
   agent subprocess:

   ```toml
   [execution.env]
   pass = ["CURSOR_API_KEY"]
   ```

Orbit deliberately does not add `CURSOR_API_KEY` to the provider's required
environment and never places a key in argv. Credential values therefore do
not enter task artifacts, audit argv, transcripts, or spawn errors; the
operator must opt in through the same child-environment policy used by other
secrets. Login itself is an unsandboxed setup action, not part of a workflow
turn.

### Model selection

Every Cursor invocation receives `--model <id>` from its crew assignment. The
shipped crew uses the model id shown by the current CLI help, `gpt-5`:

```toml
[crews.cursor]
model = "gpt-5"
provider = "cursor"
```

Use `cursor-agent models` (or `cursor-agent --list-models` on versions that
advertise that flag) to inspect the ids available to the logged-in account.
Choosing an Anthropic, OpenAI, Google, or Cursor model never changes the
provider identity: the run remains a `cursor` run with Cursor authentication,
state, audit attribution, and sandbox policy.

### Headless execution, output, and sandbox

The shipped direct-agent executor uses `--print --force --output-format json`.
Print mode is non-interactive, `--force` lets the agent apply edits and commands
without blocking for approval, and the enclosing Orbit macOS/Linux sandbox
remains authoritative. The flag cannot grant a path the OS sandbox denied.

The Orbit prompt travels on standard input, never as a positional argument.
On success, Cursor emits one JSON object with `type: "result"`,
`subtype: "success"`, `is_error: false`, and the assistant response in its
`result` string. Orbit validates that terminal wrapper before reading the inner
response envelope. A non-zero exit, malformed object, missing field, non-string
result, or absent Orbit completion envelope fails closed.

Only an active Cursor executor receives write access to `$HOME/.cursor` for
login state, CLI settings, permissions, and sessions. Other providers do not
inherit that write grant. The worktree and all other paths remain governed by
the activity filesystem profile.

---

## Pi CLI

The `pi` provider launches the local `pi` binary (npm package
`@earendil-works/pi-coding-agent`) as an Orbit-supervised worker. Everything
below was verified against **Pi 0.85.1**: the README option tables,
`src/cli/args.ts`, `src/modes/print-mode.ts`, and `docs/json.md`.

### Installation and detection

```sh
npm install -g @earendil-works/pi-coding-agent
pi --version
```

Ensure the install directory is on `PATH` before running `orbit init`. Fresh
init detects `pi`, adds a `[crews.pi]` assignment, and can choose it only after
every previously supported family in the preference order. Selecting `pi` when
its binary is unavailable fails with a permanent diagnostic naming `pi`; Orbit
never falls back to another provider.

### Authentication and credential handling

Pi supports two local authentication paths, and Orbit changes neither:

1. Run `pi` once interactively and authenticate with its `/login` command. The
   resulting credentials live under Pi's agent directory
   (`$PI_CODING_AGENT_DIR`, default `$HOME/.pi/agent`).
2. Export a vendor API key and explicitly pass it to the agent subprocess:

   ```toml
   [execution.env]
   pass = ["ANTHROPIC_API_KEY"]
   ```

Orbit never renders Pi's `--api-key` flag and adds no `*_API_KEY` to the
provider's required environment, so credential values do not enter task
artifacts, audit argv, transcripts, or spawn errors. The operator opts in
through the same child-environment policy used by every other secret. Logging
in is an unsandboxed setup action, not part of a workflow turn.

### Model and thinking selection

Every Pi invocation receives `--model <pattern>` from its crew assignment.
`--model` takes a *pattern*, which may carry a `provider/id` prefix, so the
underlying vendor is selected inside the model string rather than through a
second Orbit knob:

```toml
[crews.pi]
model = "sonnet"
provider = "pi"

# Or pin the vendor explicitly:
# model = "openai/gpt-4o"
```

Run `pi --list-models` to inspect the ids available to the authenticated
account. **Choosing an Anthropic, OpenAI, or Google model never changes the
provider identity**: the run remains a `pi` run with Pi authentication, state,
audit attribution, and sandbox policy. Correspondingly, `anthropic`, `openai`,
`google`, and `xai` remain deprecated aliases for *other* Orbit lanes and never
resolve to `pi`.

A crew `effort` is rendered as Pi's `--thinking <level>`. Pi validates that flag
against a fixed, model-independent set — `off, minimal, low, medium, high,
xhigh, max` — and rejects anything else with a diagnostic instead of ignoring
it. Orbit's crew vocabulary (`low, medium, high, xhigh, max`) is a strict subset
of that set, so every admissible crew effort reaches the CLI intact and an
inadmissible one is refused at config load. Orbit renders `--thinking` as its
own flag rather than using Pi's `<model>:<thinking>` shorthand, so the two crew
fields stay independently readable in argv and audit records. Omitting `effort`
omits the flag and leaves Pi's own default in place.

### Headless execution, output, and session isolation

The shipped direct-agent executor uses
`--mode json --no-session --no-approve --offline`:

- `--mode json` is Pi's non-interactive JSONL event stream.
- `--no-session` makes every Orbit invocation an ephemeral session. Without it
  Pi writes a session JSONL per run under its agent directory, and a later
  `--continue` could resurrect one run's context inside another.
- `--no-approve` denies project trust for the run rather than inheriting an
  ambient decision from `~/.pi/agent/trust.json` or `defaultProjectTrust`. Pi
  executes project-local `.pi` extensions once a checkout is trusted, and a
  managed worktree is repository content Orbit does not vouch for. Context files
  (`AGENTS.md` / `CLAUDE.md`) load *before* the trust decision, so repository
  instructions still reach the agent. An operator who wants project-local Pi
  resources overrides this on their own executor definition.
- `--offline` suppresses Pi's startup network calls — the `pi.dev` version
  check, package update checks, and the install/update telemetry ping. Model API
  traffic is unaffected. Orbit sends no other outbound message on Pi's behalf.

The Orbit prompt travels on standard input, never as a positional argument: Pi
merges piped stdin into the initial prompt in every non-interactive mode.

Orbit reads only terminal assistant `message_end` frames — the event Pi documents
as the final authoritative message. The latest such frame controls completion:
Orbit takes its `text` content blocks only when it is a clean answer, while a
later failed, empty, or malformed assistant terminal frame invalidates earlier
completion evidence. Everything else is dropped before any protocol read. That
reduction is a correctness requirement, not tidying: Pi's `agent_end` frame
replays the entire conversation including the user turn, and every Orbit prompt
embeds a literal example response envelope, so a reverse envelope scan over the
raw stream could read Orbit's own instructions back as the agent's completion
evidence. `thinking` and `toolCall` blocks are dropped for the same reason. A
non-zero exit, malformed JSONL, a `stopReason` of `error`/`aborted`, a stream
with no terminal frame, or an absent Orbit completion envelope all fail closed.

Because the reduction drops Pi's streaming control plane, Orbit does **not**
claim provider-reported token usage for a Pi run; the invocation trace carries
only what the Orbit response envelope itself declares. The raw stdout and stderr
captures are still written to the audit blob store unmodified, so the full
session log — including Pi's own authentication and policy diagnostics — remains
available to an operator.

### Tool integration: no native MCP

**Pi ships no MCP client** ("No MCP" is an explicit design position in its
README; MCP support would have to come from a third-party extension). Orbit does
not inject one and does not claim to.

This costs nothing for Orbit's own tools. A Pi run reaches them the same way
every CLI agent path does: Orbit puts the dispatching `orbit` binary first on
the child's `PATH` and exports `ORBIT_BIN`, and the execution envelope instructs
the agent to call `orbit tool run <tool.name> --input '<json>'` through Pi's
built-in `bash` tool. The same allowlist and caller-role gates apply as on the
MCP surface, so the activity's tool grant is enforced identically.

The practical limits, stated plainly:

- Pi cannot receive an MCP-native tool schema, so tool discovery is what the
  envelope names rather than a protocol-level list.
- `orbit mcp setup` has no Pi client to configure and does not offer one.
- An activity that grants `proc.spawn` must keep `bash` available; a crew that
  narrowed Pi's tools with `--tools`/`--no-tools` on a custom executor
  definition would cut off the Orbit tool path entirely.

### Sandbox

Only an active Pi executor receives write access to Pi's agent directory —
`$PI_CODING_AGENT_DIR` when set, otherwise `$HOME/.pi` — for login credentials,
settings, saved trust decisions, installed packages, and session state. Other
providers do not inherit that grant, and Pi does not inherit theirs. The
worktree and all other paths remain governed by the activity filesystem profile;
Pi has no OS-level sandbox flag of its own, so the enclosing Orbit
macOS/Linux sandbox is the only filesystem boundary and remains authoritative.

---

## Antigravity CLI

The `antigravity` provider launches the local `agy` binary as an
Orbit-supervised worker. This is Google's current terminal agent after the
Gemini CLI transition for individual accounts (2026-06-18). It is **not** a
rename of the `gemini` executor: the protocol, flags, MCP config, and model
slugs are different.

Gemini remains a **model family**. `gemini-*` model ids still attribute as
`gemini`. `provider = "gemini"` still means the legacy Gemini CLI, which
enterprise Gemini Code Assist and API-key deployments continue to support.
Do not treat every Gemini API deployment as shut down.

### Installation and detection

Install from [Antigravity CLI](https://www.antigravity.google/docs/cli/headless/)
and verify:

```sh
agy --version    # this change was verified against 1.1.27
agy models
```

Ensure the installed directory is on `PATH` before `orbit init`. Fresh init
detects `agy`, adds a `[crews.antigravity]` assignment, and prefers it over
the legacy Gemini CLI when both binaries are present. Selecting `antigravity`
when `agy` is missing fails with a permanent diagnostic naming `agy`; Orbit
never falls back to `gemini` or another vendor.

### Authentication and credential handling

Authenticate once with an interactive `agy` session. Headless runs use that
cached login under `$HOME/.gemini/antigravity-cli/`. Orbit never runs `agy`
login, never rewrites credentials, and does not pass API keys on argv. A
non-interactive run that is not already authenticated exits with an
`authentication required` error instead of hanging. Permission denials in
headless mode are printed to stderr and name the tool plus how to allow it.

### Model and effort

Every Antigravity invocation receives `--model <slug>` from its crew. The
shipped crew uses a slug from `agy models`:

```toml
[crews.antigravity]
model = "gemini-3.8-flash-high"
provider = "antigravity"
```

Official headless docs list `--effort low|medium|high`. `xhigh` and `max` fail
closed with migration guidance; they are not dropped or remapped. Bare Gemini
CLI ids such as `gemini-3.8-flash` also fail closed: use a slug from
`agy models`. Unknown `--model` values fail at the CLI rather than falling
back. Running a Claude or GPT slug through `agy` does not change the
execution-lane identity: the run remains `antigravity`.

Migrate an old crew with `provider = "gemini"` by changing the provider to
`antigravity` and the model to a current `agy models` slug. Customized
`gemini` executor definitions and credentials are left intact.

### Headless execution, output, MCP, and sandbox

The shipped executor uses `--input-format stream-json --output-format
stream-json --dangerously-skip-permissions`. The Orbit prompt is one
documented stdin `user` event, then stdin is closed, so the envelope never
enters argv. `--dangerously-skip-permissions` is the unattended analog of
interactive Ask; Orbit's OS sandbox remains the filesystem/network boundary.
Do not copy Gemini CLI flags (`--approval-mode yolo`, `-o json`,
`--allowed-mcp-server-names`). Do not pass `agy --sandbox`; the outer Orbit
sandbox is authoritative.

`agy --print-timeout` defaults to five minutes. Orbit always passes an
explicit `--print-timeout` derived from the remaining activity wall-clock
deadline minus a 30-second shutdown margin, so a three-hour activity is not
cut off at five minutes. A custom executor that already sets the flag keeps a
shorter value and is capped if it exceeds the derived budget; the flag is
never duplicated. Orbit's outer process timeout and cleanup remain
authoritative if the CLI ignores the flag.

On success `agy` emits a terminal `result` with `status: "SUCCESS"`, the
assistant text in `response`, and token counts in `usage`. Orbit rejects
`ERROR` / malformed / missing terminal objects as missing completion evidence.
When `agy` exits non-zero with empty stderr and a terminal `ERROR` (for
example `timeout waiting for response`), Orbit surfaces that bounded, redacted
`error` string in run/task diagnostics and does not copy `response` or prompt
text into the message.
MCP for Antigravity is configured at `~/.gemini/config/mcp_config.json`
(home) or `.agents/mcp_config.json` (workspace), not the legacy Gemini
`.gemini/settings.json` `mcpServers` map.

`$HOME/.gemini` is already a sandbox write grant (shared with the legacy
Gemini CLI). Other providers do not receive extra Antigravity-only roots.

---

## OpenCode CLI

The `opencode` provider launches the local `opencode` binary as an
Orbit-supervised worker. Everything below was verified against **opencode
1.18.29**: the published [CLI reference](https://opencode.ai/docs/cli/) and the
upstream `packages/opencode/src/cli/cmd/run.ts` and
`packages/core/src/global.ts` sources. OpenCode's hosted/served modes
(`opencode serve`, `--attach`, `opencode web`) are not used.

### Installation and detection

```sh
opencode --version   # this change was verified against 1.18.29
opencode models
```

Fresh init detects `opencode`, adds a `[crews.opencode]` assignment, and can
choose it only after every previously supported family in the preference order,
so adding this lane cannot change what an already-provisioned host picks.
Selecting `opencode` when its binary is unavailable fails with a permanent
diagnostic naming `opencode`; Orbit never substitutes another provider.

### Authentication and credential handling

OpenCode supports two local authentication paths, and Orbit changes neither:

1. Run `opencode auth login` once. The resulting credentials live in
   `auth.json` under OpenCode's XDG **data** directory — `$XDG_DATA_HOME/opencode`,
   default `$HOME/.local/share/opencode`.
2. Export a vendor API key and explicitly pass it to the agent subprocess:

   ```toml
   [execution.env]
   pass = ["ANTHROPIC_API_KEY"]
   ```

Orbit adds no `*_API_KEY` to the provider's required environment and never puts
a credential on argv. An interactive `opencode auth login` is a separate,
unsandboxed setup action that no Orbit workflow turn performs.

### Model and variant selection

Every OpenCode invocation receives `--model <provider>/<model>` from its crew
assignment. The coordinate must be **fully qualified**: OpenCode splits on the
first `/` and looks the leading segment up in its own provider catalog, so a
bare model id does not resolve.

```toml
[crews.opencode]
model = "anthropic/claude-sonnet-4-5"
provider = "opencode"
effort = "high"
description = "OpenCode through its local CLI"
tags = ["implementation"]
```

The crew chooses the `opencode` executor. The `anthropic/` prefix names the
**model vendor inside the OpenCode lane** and **never changes the provider
identity**: the run remains an `opencode` run for its authentication, state
directories, audit attribution, and sandbox policy. Orbit renders no separate
provider flag, and `anthropic`, `openai`, and `google` continue to resolve to
their own executors rather than to `opencode`. Run `opencode models` to inspect
the coordinates available to the authenticated account.

A crew `effort` is rendered as OpenCode's `--variant <value>`, documented as
"model variant (provider-specific reasoning effort, e.g., high, max, minimal)".
Because OpenCode forwards that value verbatim to whichever model provider
`--model` selected and publishes no provider-independent vocabulary, Orbit
admits only `high` and `max`. `low`, `medium`, and `xhigh` are **rejected at
configuration load** with a diagnostic — they are not remapped onto `minimal` or
`high`, and they are never silently dropped at spawn. Omitting `effort` omits
the flag and leaves OpenCode's own default in place.

### Headless execution, output, and permissions

The shipped executor supplies only static non-interactive flags:

- `run` is the non-interactive subcommand; without it the CLI starts its TUI.
- `--format json` is OpenCode's raw event stream: one JSON object per line,
  shaped `{type, timestamp, sessionID, ...}`.
- `--auto` auto-approves permission requests that are not explicitly denied.
  This is **required** for unattended runs: without it OpenCode *auto-rejects*
  every request and the turn cannot touch the worktree. It is the documented
  non-interactive analog of an interactive approval, **not** a security
  boundary — see [Sandbox](#sandbox-1) below.

`--continue`, `--session`, and `--share` are deliberately absent: the first two
would let one run's context resurface inside another, and `--share` publishes
the session. Every Orbit invocation is a fresh session.

The Orbit prompt travels on standard input, never as a positional argument.
OpenCode reads piped stdin whenever stdin is not a TTY and uses it as the whole
message when no positional `[message..]` is supplied, which keeps the execution
envelope out of process listings, audit argv, and spawn diagnostics.

Orbit reads only the assistant `text` parts of the event stream, concatenated in
order. Dropping the rest is a correctness requirement, not tidying: `tool_use`
frames carry full tool input and output, so an agent that reads its own task
record or echoes the prompt through a shell tool replays Orbit's own execution
envelope — including the literal example envelope in the response contract —
inside a tool payload, and `reasoning` frames are the model thinking aloud,
where a draft envelope is not an answer. A terminal `error` event clears any
answer text already accumulated, so a partially written envelope from before a
failure cannot be projected as success. OpenCode also exits non-zero on session
failure, and the v2 runner fails closed on a non-zero exit even when stdout
contains success-looking JSON; exit status and envelope are independent
evidence and both must be valid.

Because the reduction drops OpenCode's `step_finish` frames, Orbit does **not**
claim provider-reported token usage for an OpenCode run; the invocation trace
carries whatever the Orbit response envelope itself declares. Full stdout and
stderr are still captured in the run's audit record, so OpenCode's own
diagnostics remain available for debugging. `--print-logs` is not passed;
OpenCode writes logs to its data directory's `log/` tree.

### Tool integration: MCP is not auto-configured

OpenCode has a native MCP client configured through its own `opencode.json`.
**Orbit does not write, merge, or manage that file**, and `orbit mcp setup` does
not offer an OpenCode target. An operator who wants OpenCode's MCP client
pointed at an Orbit server configures it themselves.

This costs nothing for Orbit's own tools. An OpenCode run reaches them the same
way every non-MCP lane does: the execution envelope directs the agent to call
`orbit tool run <tool.name> --input '<json>'` through OpenCode's shell tool,
using the `orbit` binary supplied on its `PATH`. Tool grants and caller-role
gates are still enforced by `orbit tool run`, so the activity's scoped authority
is unchanged. Two consequences follow:

- OpenCode cannot receive an MCP-native tool schema for Orbit's tools, so tool
  discovery is what the envelope states rather than a negotiated list.
- The route depends on OpenCode's shell tool being available. An operator who
  has disabled it on a custom executor or in `opencode.json` breaks Orbit tool
  access for that lane.

### Sandbox

Only an active OpenCode executor receives write access to OpenCode's XDG roots:
`$XDG_DATA_HOME/opencode` (default `$HOME/.local/share/opencode`, holding
`auth.json`, the session and message stores, and logs), the config root
(`$OPENCODE_CONFIG_DIR`, else `$XDG_CONFIG_HOME/opencode`, else
`$HOME/.config/opencode`), `$XDG_STATE_HOME/opencode`, and
`$XDG_CACHE_HOME/opencode`. All four are granted because OpenCode creates the
data, config, and state roots during startup — before it ever reads Orbit's
envelope. Other providers do not inherit that grant, and OpenCode does not
inherit theirs.

The worktree and all other paths remain governed by the activity filesystem
profile. `--auto` cannot grant filesystem access the enclosing sandbox denies:
the Orbit macOS/Linux sandbox is the only filesystem boundary and remains
authoritative.

---

## Per-task crew override

`[workflow].default_crew` is the workspace fallback, not a global verdict. **Every task carries an optional `crew` field**, and `orbit run ship` resolves which crew to dispatch per task by the [resolution precedence](#resolution-precedence) above:

1. explicit `--crew` / run-input `crew`, otherwise
2. `task.crew` if set on the task artifact, otherwise
3. `[workflow].default_crew` from `config.toml`, otherwise
4. `CONSTELLATION_DEFAULT_PROVIDER` if set (environment tier), otherwise
5. the canonical `claude` system-default crew.

This means you can mix-and-match in a single ship run: route a tricky refactor to `claude` while routing routine cleanups to `codex` — both go through the same `orbit run ship` invocation, each picking its own crew at dispatch time. `orbit run ship` fans singleton child runs, so each task's `crew` is recorded on that child (`orbit run show` → `resolved_crew`) and used by `implement_one`. A single child pipeline whose `task_ids` name more than one distinct crew (or mix set and unset crews) fails closed rather than inheriting `[workflow].default_crew`.

### Automatic crew pools by complexity

Auto drains can randomly select a crew for each task that has no explicit
`task.crew`:

```sh
orbit run auto --medium-complexity-crews grok,terra
```

The equivalent configuration is:

```toml
[workflow]
low_complexity_crews = ["luna"]
medium_complexity_crews = ["grok", "terra"]
hard_complexity_crews = ["astra"]
```

Use `--low-complexity-crews`, `--medium-complexity-crews`, and
`--hard-complexity-crews` for run overrides, including with `--grant`. Each
provided CLI pool replaces only its matching configuration pool for that
drain. Configuration arrays replace their corresponding global arrays when
specified in the workspace file. Set/get/show use the same fields:

```sh
orbit config set workflow.medium_complexity_crews '["grok", "terra"]'
orbit config get workflow.medium_complexity_crews
orbit config show
```

For automatic task admission the order is an explicit run-input crew, an
explicit task crew, the matching nonempty complexity pool, then the existing
default crew resolution chain. Low, medium and hard are the task complexity
values; unset or `unassessed` complexity uses the default chain. An omitted
pool inherits configuration; an absent or empty effective pool uses the
default chain. `medium_complexity_crews = []` disables that configured pool;
`orbit run auto --medium-complexity-crews` (with no names) disables it for one
drain. Blank entries such as `""` and unknown crew names fail before dispatch.
Names are trimmed, resolved against the configured registry and deduplicated,
so repeated entries never add random weight.

Pools are selection preferences. They do not install a crew allowlist or
restrict manual assignments, ordinary `run ship`, or explicit activity crews.
When a separately supplied `--allow-crew` restricts the drain, random selection
is uniform among the pool's permitted members. A disjoint pool is ineligible
and `orbit run readiness --allow-crew <crew>` diagnoses `crew_not_allowed`; an explicit task assignment
outside the allowlist is also excluded. The allowlist still applies to system
and review activities at dispatch, and operation grants keep their scope and
admission limits.

The coordinator captures effective pools in run input `auto_crew_pools`.
Each admitted leaf or epic root records `crew` and `crew_selection`, including
the task ID, complexity, source (`task.crew`, `run_input.<complexity>_complexity_crews`,
`workflow.<complexity>_complexity_crews`, `explicit`, or `default`), and eligible
pool. Inspect these with `orbit run show <RUN_ID>`. Same-task pipeline children
and retries/resumes retain the admitted selection even if configuration or
the task assignment changes later. Different tasks, including epic descendants,
receive independent draws at their own admission. No choice rewrites
`task.crew`; a newly admitted run outside the retry lineage can select again.

### Setting `task.crew`

Three equivalent surfaces:

| Surface | How |
|---|---|
| **Web dashboard** | The crew dropdown on each task card (the chevron next to `default: <crew>` in [`orbit web serve`](../README.md#quick-start)) — selecting a crew calls `orbit.task.update` under the hood. |
| **CLI** | `orbit task add --crew <name> …` at creation, or `orbit task update <id> --crew <name>` later. Pass `--crew ""` to `task update` to clear the field. `task update` is the only surface that persists the choice — a per-run crew override validates the name and logs it without writing `task.crew`, so later `orbit run ship` dispatch does not see it. |
| **MCP / agent** | `orbit.task.add` and `orbit.task.update` accept a `crew` parameter; an empty string on update clears it. Useful when an agent is filing or amending tasks programmatically. |

The dropdown label `default: codex` in the dashboard means *the task has no `crew` set* and will inherit `[workflow].default_crew`. Picking a named crew writes it onto the task and the label updates accordingly.

### What "ran" vs what "was selected"

`orbit.task.show` returns both fields when a run exists:

- `crew` — the task's own `crew` field (the *selection*).
- `resolved_crew` + `crew_model` — what was actually dispatched (the *resolution*, including default-crew fallback). Pulled from the persisted job-run record so it stays accurate even if `default_crew` is edited later.

`task.crew` is validated at write time, so you can't `orbit task add --crew <name>` with an unknown crew. The only way to end up with a stale task-level override is to delete a crew from `config.toml` after it was already written onto tasks. In that case `orbit run ship` fails fast at run start — before any agent dispatches and before the `JobRunStarted` event is emitted — so no work is wasted.

---

## `[execution.env]` — the agent subprocess environment

Every agent subprocess — bare execution, the Linux Bubblewrap sandbox, and the
macOS `sandbox-exec` sandbox alike — starts from a **cleared** environment and
receives exactly four groups of variables, and nothing else:

| Group | Contents |
|---|---|
| Baseline | `HOME`, `LANG`, `LC_ALL`, `LOGNAME`, `PATH`, `SHELL`, `TERM`, `TMPDIR`, `TZ`, `USER` — the minimum runtime context a provider CLI needs to start. `USER`/`LOGNAME` are resolved from the OS when the dispatching process has no login environment. |
| `pass` | The names you list in `[execution.env].pass`. Default: `HOME`, `PATH`, `CODEX_HOME`, `TMPDIR`, `USER` (plus `__CF_USER_TEXT_ENCODING` on macOS). |
| Provider extras | The variables the selected provider runtime declares it requires. |
| Orbit envelope | Named execution-envelope variables Orbit actually exports — not every name that starts with `ORBIT_`. The set is run, task, and session identity (`ORBIT_RUN_ID`, `ORBIT_MANAGED_RUN_CONTEXT`, `ORBIT_AGENT_NAME`, `ORBIT_AGENT_MODEL`, `ORBIT_SESSION_ID`, `ORBIT_TASK_ID`, `ORBIT_ACTIVE_TASK_ID`), locators (`ORBIT_ROOT`, `ORBIT_REGISTRY_ROOT`, `ORBIT_WORKSPACE`, `ORBIT_WORKTREE_ROOT`, `ORBIT_BIN`), activity bindings (`ORBIT_ACTIVITY_*`, `ORBIT_STEP_INDEX`, `ORBIT_TASK_ACTOR_KIND`), and `ORBIT_SEARCH_COMPANION*`. Privilege-bearing names in the same namespace (`ORBIT_OPERATOR`, `ORBIT_WORKSPACE_CLAIM_TOKEN`, `ORBIT_MCP_SSH_ACCEPTANCE`) are **not** admitted. `ORBIT_REGISTRY_ROOT` is emitted only for a managed child and locates the authoritative global registry without changing workspace discovery. `ORBIT_WORKSPACE` is the trusted logical `ws_*` selector for nested `orbit tool run` and `orbit mcp serve` calls; it is honored only together with managed-run provenance and does not infer ownership from a linked-worktree cwd. An explicit `--workspace` or tool-payload selector still wins and still fails closed. The runner removes an inherited `ORBIT_ROOT` from that child: `ORBIT_ROOT` remains the operator-facing explicit data-root override, equivalent to `--root`, and pins global/shared/local roots when used on a direct command. The dispatching run's envelope values win over any inherited from an outer process. |

A variable in none of those groups is **absent** from the child, whatever it is
named. This is an allowlist, not a filter: Orbit does *not* forward "everything
that does not look like a secret". A benignly named credential —
`DATABASE_URL`, an internal service endpoint, a per-team API base URL — never
reaches an agent subprocess unless you name it in `pass`. Agent subprocesses
keep host network access, so this is the boundary that stops an accidental
disclosure from becoming exfiltration.

```toml
[execution.env]
pass = ["HOME", "PATH", "CODEX_HOME", "TMPDIR", "USER", "GITHUB_TOKEN"]
```

Adding a name to `pass` forwards it *when the dispatching process holds it*; a
listed name that is unset is simply absent rather than empty. `pass` replaces
rather than extends the built-in default, so include the baseline names you
still want, and keep credentials opt-in one at a time.

`execution.env.inherit` is not a configurable key. It was removed in ORB-00365
because a workspace `config.toml` could set `inherit = true` and — since
workspace config *replaces* global for security keys — silently flip every
agent subprocess to full inheritance. Inheritance is fixed off; a stale
`inherit` key in a config file is accepted and ignored.

---

## Other sections (brief)

| Section | Purpose |
|---|---|
| `[execution.env]` | Env vars passed to agent subprocesses. The child environment is *composed from an allowlist*, never filtered out of Orbit's own — see [`[execution.env]` — the agent subprocess environment](#executionenv--the-agent-subprocess-environment). |
| `[execution.codex]` | Codex CLI sandbox mode. Valid: `read-only`, `workspace-write` (default), `danger-full-access`. Optional `approval_policy = "on-request"` enables escalation prompts. |
| `[tasks]` | `id_start = N` sets a floor for the local task-id allocator: on runtime build the counter is raised to at least `N` (never lowered), so machines can hold disjoint id ranges (e.g. one `0–9999`, another `10000+`) and avoid cross-machine collisions. Capped by `ORB_TASK_ID_MAX` (99999) — setting it near the ceiling shrinks the usable range. Prefer the one-shot `orbit workspace init --task-id-start N` for the initial seed; the config key keeps the floor sticky across machines that share a config. See [task-migration overview](design/task-migration/1_overview.md). |
| `[scoring]` | `enabled = true` records per-agent scoreboard counters under `.orbit/state/scoreboard/`. |
| `[pr]` | PR creation defaults (template, labels, draft mode) for `orbit run ship --mode pr`. |
| `[operation]` | Operation-mode preferences [ORB-11332]: `preset` (`supervised` default / `autonomous`) plus the preset-managed `preparation`, `preparation_due_seconds`, `leaf_ceiling`, `promotion`, `completion`, `recovery`, `recovery_episodes_per_task`, `recovery_minutes_per_task`, and the independent `review_policy` (`none` default), `review_crew`, `review_reviewer_starts` (2), `review_repair_cycles` (2), `review_minutes` (30), `delivery_cap` (`review` default). `before-pr` holds PR creation for a fresh reviewer from `review_crew` on the PR route [ORB-11333]; `review_crew` applies to that reviewer only, while `after-landing` review is minted by the `delivery-code-review` auto-task with that definition's own template crew. Resolve built-in → global → workspace → run; an explicit `preset` at a layer resets the preset-managed keys before that layer's own values apply, while the independent keys keep their own precedence. Unknown keys and out-of-range values fail load. **Preferences authorize nothing**: scoped automation needs `orbit operation enable`; `orbit operation explain` shows each effective value with its winning source. See [operation-mode operations](design/operation-mode/5_operations.md). |
| `[runtime]` | **JSONL log rotation/retention** (`~/.orbit/state/logs/orbit.jsonl`): `log_retention_days` (default `7`) deletes archives older than N days; `log_max_total_mb` (default `500`) caps total archive size, pruning oldest first; `log_max_file_mb` (default `100`) rolls the active file to a dated archive once it exceeds N MiB. Rotation runs opportunistically at process start. Invalid values (`0`, or `log_max_file_mb > log_max_total_mb`) are rejected at config load. |

---

## Validation and errors

Config is parsed at startup; invalid entries fail loud rather than silently falling back. Common failure modes:

- `[workflow].default_crew = '<x>' is not defined under [crews]` — name a crew that exists.
- `config schema changed in ORB-00058; remove [agent.<role>] tables` — migrate to crews.
- `execution.codex.sandbox has invalid value` — must be `read-only`, `workspace-write`, or `danger-full-access`.
- `tasks.id_start N exceeds maximum task id 99999` — the allocator start must fit the `ORB-00000` id space.
- `tasks.id_start N would lower the allocator below its current position M` — the counter only moves forward (raised only via `orbit workspace init --task-id-start`; the config key is a silent forward-only floor).
- `[operation] has unknown key 'x'` / `operation.preset has invalid value 'fast'` — operation-mode keys are typed and closed; a misspelled or unknown setting is refused rather than silently resolving to a different authority statement.

The runtime parser intentionally accepts sections owned by other readers of the shared file, such as `[docs]`. Consequently, retired keys with no runtime reader can remain syntactically accepted but have no effect. Existing configs containing `[duel]` and `[duel.models]` still load during the compatibility window and emit a warning naming both retired tables; remove them. The keys `execution.env.inherit`, `task.approval.delegate_approval`, and `task.approval.required_for_agent` are also inert and should be removed; environment inheritance is fixed off, while agent approval is enforced by the capability/policy surfaces rather than these old flags.

When in doubt, start with a minimal workspace file containing only genuine overrides. The annotated default ([`crates/orbit-config/assets/default-config.toml`](../crates/orbit-config/assets/default-config.toml)) is a reference for available settings, not a template that must be copied wholesale.
