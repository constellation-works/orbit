---
type: context
summary: Orbit Configuration
last_validated: 2026-08-17
---

# Orbit Configuration

Reference for Orbit's runtime config — the `config.toml` consumed by `orbit run ship` and the activity-job dispatcher. The defaults shipped with the binary live in [`crates/orbit-config/assets/default-config.toml`](../crates/orbit-config/assets/default-config.toml).

This doc focuses on the user-facing knobs: `[workflow]` and `[crews.*]`. Other sections are summarized at the end.

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

  **The `system` crew.** Interactive `orbit init` (or `--force` on a fresh rewrite) asks which detected bounded family should back `[crews.system]`: Codex Luna (`gpt-5.6-luna`), Claude Sonnet, Gemini Flash (`gemini-3.8-flash`), Grok (`grok-4.6`), Copilot Haiku, or Cursor (`gpt-5`; Cursor has no stable cheap-tier alias). It does not offer Astra, Sol, Opus, Terra, or a free-form custom provider, and it never prompts for a QA crew. `workflow.system_crew` stays `system`; only the assignment behind that name is chosen. A host with exactly one of those families auto-accepts it; a host with none omits `[crews.system]` rather than inventing a provider. `--non-interactive` never prompts and still auto-seeds `[crews.system]` from the preference order: Codex Luna, then Claude Sonnet, then Grok, then Gemini Flash, then Copilot, then Cursor. Appending the newer lanes preserves every existing family's selection. To change what runs system work after init, edit `[crews.system]`. Configs written before this crew existed have no such table, so the name is resolved first onto the crew `system_crew` names when that crew exists. For Orbit's default or legacy lane names (`system` and `qa`), a missing crew falls back to an existing `qa` crew and then to the already-validated workspace default; the latter keeps old Gemini- and Grok-only configs working even though they never seeded `qa`. Unknown custom names are not substituted. A host that points `system_crew` at a defined cheap crew therefore keeps running system work there rather than being silently relocated. An explicit `[crews.system]` always wins. `[crews.qa]` remains a loadable compatibility lane for explicitly user-authored legacy configs, but fresh init never creates it.

---

## `[crews.<name>]` — which provider-model runs the task

A **crew** is one provider-model assignment. Activities do not carry a model-selection role: a rendered activity input may name a `crew`, and otherwise the activity inherits the run's resolved crew.

| Field | Purpose | Values |
|---|---|---|
| `model` | Model identifier passed to the provider CLI | Provider-specific (e.g. `opus`, `sonnet`, `gpt-6-astra`, `gemini-3.8-flash`, `grok-4.6`) |
| `provider` | Agent family | `claude`, `codex`, `gemini`, `grok`, `copilot`, `cursor` (the CLI-executable crew families; see [Provider identity and resolution](#provider-identity-and-resolution) for the full canonical set) |
| `effort` | Optional provider reasoning effort | Claude/Codex: `low`, `medium`, `high`, `xhigh`, or `max`; Grok: verified per model below |
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
`model_reasoning_effort`, Claude receives `--effort`, and Grok receives
`--reasoning-effort`. The installed Claude CLI (2.1.261, checked September
2026) advertises all five values, so Orbit forwards `low`, `medium`, `high`,
`xhigh`, and `max` exactly; [Claude's effort documentation](https://code.claude.com/docs/en/model-config)
notes that availability can still depend on the selected model. Grok Build
1.0.13 advertises `--reasoning-effort` (with `--effort` as an alias), and
`grok models` currently lists `grok-4.6` and `grok-4.5`. Orbit accepts `low`,
`medium`, `high`, and `xhigh` for `grok-4.6`, and `low`, `medium`, and `high`
for `grok-4.5`, matching [xAI's reasoning contract](https://docs.x.ai/developers/model-capabilities/text/reasoning).
It rejects `max`, unsupported Grok model/effort pairs, and effort on other
providers clearly rather than silently downgrading or ignoring a request. A
selected activity crew (including `workflow.system_crew`) supplies its effort
together with its provider and model, so it takes precedence over an activity's
inline baseline in the same way as the rest of that assignment. For the
standard Codex tiers, use the model-specific crew to choose capability first:
Terra (`gpt-5.6-terra`) is the medium-low crew; `effort` adjusts the reasoning
budget inside the chosen Codex model.

Example — the standard Grok crew:

```toml
[crews.grok]
model = "grok-4.6"
provider = "grok"
```

The current Grok Build CLI lists `grok-4.6` as its default from `grok models`, so Orbit uses that live menu id. The older `grok-build` string is not retained as a default or alias.

Fresh `orbit init` configuration advertises only detected provider CLIs. Claude seeds `opus`, `sonnet`, and `fable`; Codex seeds `astra`, `sol`, `terra`, and `luna`; Gemini seeds `gemini`; Grok seeds `grok`; Copilot seeds `copilot`; and an installed `cursor-agent` seeds `cursor`. Copilot and Cursor are appended after the original four families, so installing either never changes the default crew, default provider, or system crew on a host that already has an earlier family. Interactive init still asks for the default crew (`[crews.custom]`) and, separately, for the system crew written as `[crews.system]`; it does not ask for QA. `--non-interactive` auto-seeds `[crews.system]` from the preference order above whenever a supported family is detected. The legacy `qa` name remains loadable when an existing user-authored config defines `[crews.qa]`, but init does not seed that table. If no supported provider CLI is detected, init leaves both the crew registry and `workflow.default_crew` unset instead of writing an unusable provider.

The `astra` crew (`gpt-6-astra`) is the Codex fresh-config default; the existing `sol`, `terra`, and `luna` crews remain available. The Gemini `gemini` crew uses `gemini-3.8-flash`. These exact IDs are the current provider-advertised model codes: [GPT-6 Astra](https://developers.openai.com/api/docs/models/gpt-6-astra) and [Gemini 3.8 Flash](https://ai.google.dev/gemini-api/docs/models/gemini-3.8-flash). Existing explicit model pins are retained when their configuration loads.

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

- **Parsing is case- and whitespace-insensitive.** `Claude`, `  claude `, and `CLAUDE` all resolve to `claude`. `openai-compat` normalizes to `openai_compat`.
- **Deprecated aliases resolve *and* warn.** The legacy vendor names normalize to their canonical id and log an `orbit.config.crew` deprecation warning (`{alias, canonical}`) — they never fail, but update the config:

  | Deprecated alias | Canonical |
  |---|---|
  | `anthropic` | `claude` |
  | `openai`, `chatgpt` | `codex` |
  | `google` | `gemini` |
  | `xai` | `grok` |

  `copilot` and `cursor` have **no** aliases. `github`, `cursor-agent`, and
  `anysphere` are not provider spellings, and the vendor that supplies a
  session's underlying model never changes its execution-lane identity. See
  [GitHub Copilot CLI](#github-copilot-cli) and
  [Cursor Agent CLI](#cursor-agent-cli).

- **Canonical ≠ Worker-executable.** Orbit's canonical set is deliberately wider than what the model-neutral Worker leaf executor can run: `copilot`, `cursor`, `ollama`, and `openai_compat` are first-class Orbit providers but Worker does not execute them. This distinction is preserved on purpose — do not narrow the canonical set to Worker's subset. For `copilot` and `cursor` this is a *stable diagnostic*, not a fallback: a Worker-routed step naming either lane is refused by identity rather than silently re-pointed at another family.
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
- `unknown provider '<x>'; expected one of claude, codex, gemini, grok, copilot, ollama, openai_compat, cursor — no CLI runtime registered` — the provider string did not resolve to a canonical id.

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

## Per-task crew override

`[workflow].default_crew` is the workspace fallback, not a global verdict. **Every task carries an optional `crew` field**, and `orbit run ship` resolves which crew to dispatch per task by the [resolution precedence](#resolution-precedence) above:

1. explicit `--crew` / run-input `crew`, otherwise
2. `task.crew` if set on the task artifact, otherwise
3. `[workflow].default_crew` from `config.toml`, otherwise
4. `CONSTELLATION_DEFAULT_PROVIDER` if set (environment tier), otherwise
5. the canonical `claude` system-default crew.

This means you can mix-and-match in a single ship run: route a tricky refactor to `claude` while routing routine cleanups to `codex` — both go through the same `orbit run ship` invocation, each picking its own crew at dispatch time. `orbit run ship` fans singleton child runs, so each task's `crew` is recorded on that child (`orbit run show` → `resolved_crew`) and used by `implement_one`. A single child pipeline whose `task_ids` name more than one distinct crew (or mix set and unset crews) fails closed rather than inheriting `[workflow].default_crew`.

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
| `ORBIT_*` | Orbit's own execution envelope — run, task, and session identity, `ORBIT_REGISTRY_ROOT`, `ORBIT_WORKSPACE`, and `ORBIT_BIN`. `ORBIT_REGISTRY_ROOT` is emitted only for a managed child and locates the authoritative global registry without changing workspace discovery. `ORBIT_WORKSPACE` is the trusted logical `ws_*` selector for nested `orbit tool run` and `orbit mcp serve` calls; it is honored only together with managed-run provenance and does not infer ownership from a linked-worktree cwd. An explicit `--workspace` or tool-payload selector still wins and still fails closed. The runner removes an inherited `ORBIT_ROOT` from that child: `ORBIT_ROOT` remains the operator-facing explicit data-root override, equivalent to `--root`, and pins global/shared/local roots when used on a direct command. The dispatching run's envelope values win over any inherited from an outer process. |

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
| `[runtime]` | **JSONL log rotation/retention** (`~/.orbit/state/logs/orbit.jsonl`): `log_retention_days` (default `7`) deletes archives older than N days; `log_max_total_mb` (default `500`) caps total archive size, pruning oldest first; `log_max_file_mb` (default `100`) rolls the active file to a dated archive once it exceeds N MiB. Rotation runs opportunistically at process start. Invalid values (`0`, or `log_max_file_mb > log_max_total_mb`) are rejected at config load. |

---

## Validation and errors

Config is parsed at startup; invalid entries fail loud rather than silently falling back. Common failure modes:

- `[workflow].default_crew = '<x>' is not defined under [crews]` — name a crew that exists.
- `config schema changed in ORB-00058; remove [agent.<role>] tables` — migrate to crews.
- `execution.codex.sandbox has invalid value` — must be `read-only`, `workspace-write`, or `danger-full-access`.
- `tasks.id_start N exceeds maximum task id 99999` — the allocator start must fit the `ORB-00000` id space.
- `tasks.id_start N would lower the allocator below its current position M` — the counter only moves forward (raised only via `orbit workspace init --task-id-start`; the config key is a silent forward-only floor).

The runtime parser intentionally accepts sections owned by other readers of the shared file, such as `[docs]`. Consequently, retired keys with no runtime reader can remain syntactically accepted but have no effect. Existing configs containing `[duel]` and `[duel.models]` still load during the compatibility window and emit a warning naming both retired tables; remove them. The keys `execution.env.inherit`, `task.approval.delegate_approval`, and `task.approval.required_for_agent` are also inert and should be removed; environment inheritance is fixed off, while agent approval is enforced by the capability/policy surfaces rather than these old flags.

When in doubt, start with a minimal workspace file containing only genuine overrides. The annotated default ([`crates/orbit-config/assets/default-config.toml`](../crates/orbit-config/assets/default-config.toml)) is a reference for available settings, not a template that must be copied wholesale.
