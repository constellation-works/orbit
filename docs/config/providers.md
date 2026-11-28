# Providers and executors

## Provider identity and resolution

Every `provider` string (a crew's, an activity's inline `provider`, setup detection) goes through one canonical parser.

### Canonical providers

| ID | CLI binary | Notes |
|---|---|---|
| `claude` | `claude` | |
| `codex` | `codex` | |
| `gemini` | `gemini` | Legacy Gemini CLI (enterprise / API-key deployments). |
| `grok` | `grok` | |
| `copilot` | `copilot` | [GitHub Copilot CLI](#github-copilot-cli) |
| `antigravity` | `agy` | [Antigravity CLI](#antigravity-cli) |
| `muse` | `muse` | [Muse Code CLI](#muse-code-cli) |
| `ollama` | — | Recognized but unsupported at the Orbit CLI entry point. |
| `openai_compat` (`openai-compat`) | — | HTTP-only, no CLI runtime; selecting it fails. |

- Parsing ignores case and surrounding whitespace.
- Deprecated aliases resolve with an `orbit.config.crew` warning: `anthropic` → `claude`, `openai`/`chatgpt` → `codex`, `google` → `gemini`, `xai` → `grok`.
- `copilot`, `antigravity` and `muse` have no aliases, and the model vendor a lane runs (a Claude model through Copilot) never changes the provider.
- The cross-repo Worker executor runs only `claude`, `codex`, `gemini` and `grok`; a Worker-routed step naming another lane is refused, not re-pointed.

**Audit attribution.** Process audit maps `ORBIT_AGENT_NAME` and `ORBIT_AGENT_MODEL` to an agent family. An invalid pair (`copilot` with a `claude-*` model) or an agent envelope with no identity records `unknown` with a warning. An agent envelope (non-empty name and model, truthy `ORBIT_MANAGED_RUN_CONTEXT`, `ORBIT_TASK_ACTOR_KIND=agent`) takes precedence over `ORBIT_ACTOR`, the operator override and the OS username. This affects recorded identity only, not command authorization.

### Resolution precedence

**Which crew a task dispatches.** The first tier that is set wins:

1. **explicit**: `--crew` or run-input `crew`.
2. **task_config**: an explicit `task.crew`, or a pool or [purpose](crews.md#purpose-crew-pools) assignment still valid for its pool (normally set at creation; admission redraws stale pool assignments and default fallbacks, see [pools](crews.md#automatic-crew-pools-by-complexity)).
3. **workspace_default**: `workflow.default_crew`.
4. **environment_default**: `CONSTELLATION_DEFAULT_PROVIDER`, a provider ID or alias. `claude` selects `opus`, `codex` selects `sol`, `antigravity` selects `gemini-flash` and `muse` selects `muse-spark` when defined; any other ID selects the same-named crew. Never overrides a configured `default_crew`.
5. **system_default**: the `opus` crew.

The environment-default tier applies even when neither config file exists.
An unset or blank variable leaves the built-in default in effect. An invalid
provider is refused during config admission unless a file configures
`workflow.default_crew`, which takes precedence over the variable.

**Which crew an activity uses.** A non-empty `crew` in its rendered input, otherwise the pool its `crew_config_key` names ([purpose](crews.md#purpose-crew-pools), complexity or final-recovery pool), otherwise the run's crew. Activity and job assets that declare `role` are rejected.

**Crew over inline baseline.** For each of `provider`, `model` and `effort`, the selected crew's value wins when present, else the activity's inline `agent_loop` value. An unrecognized crew `provider` is logged and falls back to the inline provider, so a typo never moves dispatch to another runtime. A provider already recorded on a run is reused verbatim on reconciliation.

### No silent fallback

An explicit selection that cannot run fails with a stable diagnostic and never falls back to another runtime:

- `provider openai_compat is unsupported by the Orbit CLI entry point (HTTP-only)`
- `provider ollama is unsupported by the Orbit CLI entry point`
- `unknown provider '<x>'; expected one of claude, codex, gemini, grok, copilot, ollama, openai_compat, antigravity, muse`
- `provider 'cursor' is retired and no longer supported; pick a supported provider (…)` — likewise for `pi` and `opencode` (see [retired providers](#retired-providers))
- A selected lane whose binary is missing fails permanently, naming the binary.

---

## Provider CLI notes

For every lane: Orbit passes `--model` from the crew and sends the prompt on **stdin**, never argv (process listings and audit see argv). Muse Code, which cannot read stdin, gets the prompt in a private file instead ([below](#muse-code-cli)). The Orbit OS sandbox is the filesystem boundary, and provider-specific write grants apply only while that provider runs. Credentials reach the agent through [`[execution.env].pass`](execution.md#executionenv--the-agent-subprocess-environment), which every crew shares; through a crew's global [`env`](crews.md#per-crew-credentials-env) entry, which reaches only that crew's agents; or from the provider's own credential store. Orbit never puts a key on argv. A lane without native or Orbit-managed MCP reaches Orbit tools with `orbit tool run <tool> --input '<json>'` through its shell tool, under the same grants.

`orbit tool run` selects a workspace from any directory with `workspace` in the JSON input or the global `--workspace` flag: a name, `ws_*` ID, absolute local checkout path, or local `hm_*/ws_*` selector from the federated listing. A selector naming a registered remote host (`orbit host list`) is delivered to that host over the federated route, and fails with `unreachable_destination` when the route is down. A selector naming a host this machine does not list fails locally with `invalid_input`, naming the host. Unparseable JSON or an unreadable `--input-file` is reported before workspace selection, runtime open or migration, so an input typo never migrates the checkout.

### Retired providers

The `cursor`, `pi` and `opencode` lanes were retired (ORB-18096). A crew naming one fails to load, naming the crew, the retired provider and the remedy: pick a supported provider (`claude`, `codex`, `gemini`, `grok`, `copilot`, `antigravity`, `muse`). `orbit doctor` reports the same refusal. Persisted records naming them still read and display. The Cursor editor MCP client (`orbit mcp init --cursor`) is a separate surface and is unaffected.

### GitHub Copilot CLI

| | |
|---|---|
| Install | `npm install -g @github/copilot`, then `copilot --version`. The retired `gh copilot` extension is not supported. |
| Auth | `COPILOT_GITHUB_TOKEN`, `GH_TOKEN`, `GITHUB_TOKEN`, else the `copilot /login` session (macOS keychain item `github-copilot-app`). Orbit forwards none of the token variables unless one is in `pass`. On a Mac, run `/login` once. |
| Model | `--model` is always passed, so `COPILOT_MODEL` and the saved `/model` are ignored. List IDs with `/model` inside `copilot`; recheck pins after CLI upgrades. |
| Flags | `--allow-all-tools --no-ask-user --output-format json`. Never `--allow-all`/`--yolo`, which widen paths and URLs. |
| Sandbox | Write: `COPILOT_HOME` (default `~/.copilot`) and `$XDG_CACHE_HOME/copilot`. macOS: read `~/Library/Keychains`. No access to `~/.config/gh`. |

An account without Copilot entitlement fails with `Error: Authentication failed`; org-policy-disabled third-party MCP servers produce a `session.warning` (`warningType: "policy"`). The GitHub org admin fixes both. A run with no assistant message fails the step.

### Antigravity CLI

`antigravity` launches `agy`, Google's current terminal agent: a separate lane from `gemini` (the legacy Gemini CLI), with different flags, MCP config and model slugs. `gemini-*` models still attribute to the Gemini model family.

| | |
|---|---|
| Install | See the [Antigravity CLI docs](https://www.antigravity.google/docs/cli/headless/), then `agy --version` and `agy models`. |
| Auth | One interactive `agy` login, cached under `~/.gemini/antigravity-cli/`. An unauthenticated headless run exits with `authentication required`. |
| Model | A slug from `agy models`; `effort` accepts `low`/`medium`/`high` only. To migrate a `provider = "gemini"` crew, change the provider and use an `agy models` slug. |
| Flags | `--input-format stream-json --output-format stream-json --dangerously-skip-permissions`, plus `--print-timeout` set to the activity's wall-clock runtime minus 30 s, so `agy` stops itself before Orbit's deadline kills it. The argv is fixed at launch, before any build-admission credit is earned, so credit extends Orbit's deadline but not `agy`'s budget. A shorter custom value is kept. The invocation-start audit event records `print_timeout_ms` and the launch `effective_deadline_ms`; the step output records the same print budget and the final `effective_deadline_ms` (runtime plus earned credit). Deadlines are durations from spawn. For runtimes of 30 s or less the full runtime is used, with a minimum of 1 s, because the shutdown margin does not fit. Don't add `agy --sandbox` or Gemini CLI flags. |
| Sandbox | Write: `~/.gemini` (shared with the Gemini CLI). macOS: read `~/Library/Keychains`. |
| MCP | `~/.gemini/config/mcp_config.json` or `.agents/mcp_config.json`, not `.gemini/settings.json`. |

Orbit checks the response from a terminal `result` with `status: "SUCCESS"` for its completion envelope; a non-zero exit with a terminal `ERROR` surfaces the bounded, redacted `error` string. Running for the whole `--print-timeout` spends the provider budget and is never completion: a run that then exits 0 with `SUCCESS` but no Orbit completion envelope fails as a timeout (`timed_out: true`, retryable, transient) with a diagnostic naming the print-timeout and elapsed time, keeping the bounded `final_message`. An early exit without an envelope gets the ordinary completion-envelope message.

Background commands cannot be turned off for a headless run. `agy` 1.3.3 has no flag or setting for it (`agy --help` lists none), so Orbit passes nothing and instead tells every provider, in the shared prompt contract, to run commands in the foreground. `agy` itself waits for a background task it started only until `--print-timeout`, capped at 30 minutes, then ends the turn and exits 0 with the task still running and no envelope. When the last `manage_task`-style status report in stdout shows a `task-<id>` still running, the step fails naming that task, the elapsed time and the bounded last `response`, instead of the generic message. The exit code, a `SUCCESS` wrapper and a persisted summary never count as completion; whatever the run left in the checkout is unverified.

### Muse Code CLI

`muse` runs Muse Code, Meta's terminal coding agent. The contract below was observed on Muse Code 1.4.10 (`1.4.10-R6427.1`, macOS): `muse exec --help` and offline `--provider echo` runs only, with no authenticated run. The account-window allowance was additionally observed on Muse Code 1.4.12 in an authenticated macOS run (2026-10-23).

| | |
|---|---|
| Install | `~/.local/bin/muse`, a launcher that execs `~/.local/bin/muse-bin-<version>`; check with `muse --version`. Init detects `muse` on `PATH` and enables the `muse-spark`, `muse-wonder` and `muse-wonder-full` crews. Orbit sets `MUSE_NO_AUTO_UPDATE=1` and `MUSE_LOGIN=0` for every run (an outer value cannot override them), so a run never swaps the binary under running work or starts a device login; upgrade with the launcher yourself. |
| Routing | The shipped asset sets no `host_os`: Muse runs on macOS and Linux, so a muse crew is admitted on either host and the seeded sandbox is translated per host (`macos-sandbox-exec` on macOS, `linux-bwrap` on Linux). Native Windows is not an Orbit runtime (ORB-13857), so a Windows host still has no runnable Muse lane through the normal executor/sandbox availability path. |
| Auth | `muse login` (Meta account, browser device code) once, outside Orbit. The login writes the file `auth.json` under the config root (`$XDG_CONFIG_HOME/muse`, else `~/.config/muse`), and on macOS `muse exec` also reads the login keychain, so Muse gets the read-only `~/Library/Keychains` carve-out; without it a valid login fails with `missing meta credentials`. Or `pass = ["META_API_KEY"]`, which Muse prefers; `muse auth set --api-key-stdin` also works. Orbit never logs in or forwards `META_API_KEY` unless `pass` names it. |
| Model | A catalog id (`muse-spark-1.3`); `muse model-profile show <model> [--effort <tier>]` resolves one. `effort` renders as `--reasoning-effort`; Muse accepts `none` … `ultra`, which covers every Orbit effort, and any other value is refused at config load. |
| Prompt | `muse exec` does not read stdin (piped stdin alone exits 2, `missing prompt`) and refuses `--prompt-file /dev/stdin` (`not a regular file`). Orbit writes the prompt to a mode-0600 file with an unpredictable name in the run's scratch directory (`<worktree>/.orbit/tmp/`), passes `--prompt-file <path>`, and deletes the file when the invocation ends, whatever its outcome. The envelope never reaches argv; the audit keeps the prompt in the invocation's stdin blob. An activity profile that cannot read the worktree cannot read the file either, and the run fails. |
| Flags | `exec --json --approval-mode never --disable-request-user-input --no-foreign-personal-context --disable-reminders --no-session-log`. `--approval-mode never` and `--disable-request-user-input`: nobody can answer mid-turn, and neither is a security boundary. `--no-foreign-personal-context`: Muse otherwise loads your Claude Code and Codex personal rules and skills. `--disable-reminders`: otherwise every turn spawns skill, goal and verify reminder child agents. `--no-session-log`: no session log per run (this also disables Muse's local session messaging). `--trust-workspace` is not passed: a worktree is content Orbit does not vouch for, so its Muse skills and rules stay unloaded and agent delegation stays off; the envelope directs the agent to read `AGENTS.md`. `--max-model-steps` and `--disable-web-tools` are not passed; the activity deadline bounds the run. |
| Sandbox | Write: the config root and `$XDG_DATA_HOME/muse` (else `~/.local/share/muse`: tracing, plugins, skills); on macOS also `~/Library/Application Support/Muse` (session-name authority). macOS: read `~/Library/Keychains`. Muse's own sandbox is on by default with network `proxy-only`; under Orbit's sandbox (or `sandbox: off`) Orbit appends `--disable-sandbox`, because a second Seatbelt profile cannot be applied inside Orbit's and Orbit's profile is the filesystem boundary. `--yolo` is never passed. |
| MCP | Muse has a native MCP client (`mcpServers` in `settings.json`, or `-c KEY=VALUE` per run), but per-run injection of Orbit's server was not verified, so Orbit passes no `-c` and never writes `settings.json`; Orbit tools go through `orbit tool run` on `PATH`. `orbit mcp init` has no Muse target. |
| Images | `muse exec --image <PATH>` (repeatable) attaches a local image. On Muse Code 1.4.14 (Linux) an authenticated canary probe passed for all three shipped crews, so the shipped executor declares [`image_input`](#executor-image-input-image_input) `attach_args` for PNG. |

Orbit reads only string `payload.text` of the last `run.terminal.*` record, and only when its `payload.terminal` is `completed`. On completed terminals, missing or non-string `payload.text` cannot be replaced by `payload.message`, `payload.error`, `payload.error.message` or `payload.reason`. For non-completed terminal diagnostics, Orbit keeps non-empty `payload.text` first; if it is empty or whitespace-only, fallback failure reasons are tried in this order: `payload.message`, `payload.error` (or `payload.error.message`), then `payload.reason`, with empty or whitespace-only values falling through. `failed`, `cancelled` and unknown values (the field is wire-open) fail the step, as do a missing terminal record, a stdout line that is not a JSON record with a `payload_type`, and a non-zero exit even with a completed record. The prompt echoes (`run.lifecycle.started`, `turn.input.user`), partial `run.output.delta` text and child-task records never count as completion. The terminal record carries no usage, so no provider token usage is reported. Account usage windows instead come from top-level usage records with `payload.kind = "run_usage"`: a `status` record (Muse Code 1.4.12) or, from Muse Code 1.4.13, a `run.usage` record (`payload_type` `run.usage`, `record_type` `status`, the same allowance shape). Orbit reads both and takes the last one. `payload.allowance.window` and `.weekly` report `used_percent` and `resets_at_ms` (epoch milliseconds). Orbit preserves the `window` / `weekly` labels, records them at run completion and gates them through the shared provider-limit policy; it does not infer a window duration or read per-model token usage from this record. `muse serve` (MSP over stdio) is not used.

### MCP client registration

`orbit workspace init --mcp` registers Grok through the project `.mcp.json` shared with Claude Code; `orbit mcp init --scope home --grok` uses `~/.claude.json` (both listed in Grok's [MCP compatibility documentation](https://docs.x.ai/build/features/mcp-servers)). Re-running init migrates an older Orbit-generated Grok entry out of `.grok/config.toml`, keeping other Grok settings and servers. Init keeps the native `.grok/config.toml` target when `[claude_compat] imported = true` in `~/.grok/config.toml` (Grok then ignores project `.mcp.json`), and the native home target when `[compat.claude] mcps` is disabled or `GROK_CLAUDE_MCPS_ENABLED=0`. `grok inspect` shows the loaded source. Removing the shared `orbit` registration through Claude or Grok removes it for both. Gemini CLI still uses `.gemini/settings.json` (`~/.gemini/settings.json` for home scope); its [configuration reference](https://github.com/google-gemini/gemini-cli/blob/main/docs/reference/configuration.md) does not document the shared locations.

---

## Executor `host_os`

An executor asset may declare the host operating systems it can run on, beside `sandbox` and `model_flag`:

```yaml
spec:
  host_os:
    - macos
```

- The value is a non-empty subset of `linux`, `macos` and `windows`. An empty list or any other name is refused when the asset loads, and the error names that asset. Omit `host_os`, or set it null, and the executor runs on every host. Assets that omit it load as they did before the field existed.
- A task's effective OS requirement is its `os:` tags intersected with its assigned crew's executor `host_os`. Local admission, `orbit run ship` discovery, the drain-start warning and owner pull admission all read that one intersection, so they cannot disagree. An empty intersection (an `os:linux` task assigned a crew whose executor declares `host_os: [macos]`, for example) is refused when the task is created or updated. The reason names the crew, the executor constraint and the task's tags, and nothing is written: the task never becomes one that no host can run.
- **Operator readout.** Task show (`Runs on:` and JSON `os_requirement`), task list and dashboard task details derive the same tags/stored-crew intersection without writing task fields or adding OS tags. JSON includes `any_of`, `describe`, `source` (OS `tags`, assigned `crew`, and constrained `executor` when present) and `state`. Unknown or disabled crews are `unresolved`; disjoint constraints are `conflict` and admit no host. `orbit task list --runs-on linux|macos|windows` (MCP `runs_on`) filters before pagination, includes unrestricted tasks, and excludes unresolved/conflicting requirements. With neither tags nor a constrained stored crew the JSON key is omitted. For an unset crew this is a tags-only readout: automatic pool/default routing remains undecided and can impose further OS constraints. The stored crew is used even when a past run records a different crew.
- An explicit crew this host cannot run stays assigned and waits, for example `waits for a macos host (crew <macos-only-crew>)`. It is not redrawn onto another crew. Admission draws and `workflow.default_crew` resolution skip a crew whose executor excludes this host; the other members keep their weights, and an excluded default is left unset rather than pinned. Creation-time complexity draws — task creation, `task update --crew ""`, and complexity re-rates — instead count a crew as runnable when any host serving this workspace can run it: the owner itself, plus each registered host (`~/.orbit/hosts.toml`, what `orbit host list` reports, including each host's `host_os`) whose recorded operating system the executor allows and which the owner's own claim records for this workspace show pulling here within the last seven days. Being registered is not enough: a host in the registry that serves only other workspaces — or none — never counts, and a follower that stops pulling ages out after the window and stops counting, though tasks already drawn keep their crew and wait. So a Linux owner whose Mac pull follower recently claimed work in this workspace can draw a macOS-only crew from a complexity pool; the task is stored with that crew and pool provenance, waits on the Linux owner under the same effective-OS intersection, and is claimed only by a macOS follower through owner pull admission. A crew no serving host can run is still skipped at creation, and a workspace with no serving followers draws exactly as a single host always has. Activity and purpose pools (recovery, review, pilot) run on the executing host and keep filtering by that host alone, as does every admission-time draw. A host's OS is read from it when `orbit host add` registers it (discovery reports `host_os`); an entry registered before that field existed records no OS and counts for no limited crew until it is removed and added again. If no supported purpose or complexity pool remains, local admission and the automatic draw withhold that task with a wait naming the default and the OS it needs; leaving the crew unset never permits the unsupported default to launch. Discovery for a named task uses that run's admitted crew selection, including an explicit choice retained by replay, instead of retrying the unsupported automatic default. This does not override the task's stored crew constraints or its `os:` tags. Activity default fallbacks apply the same host check, including native-inspection activities. Provider preflight stays a backstop for a missing binary or a failed login on a host the executor does allow.
- Seeding copies a shipped `host_os` onto an installed default only when that install still omits the field. A value an operator set is kept. Clearing the field restores the shipped declaration on the next seed. Custom executors outside the shipped set are not rewritten.
- `orbit doctor providers` reports `unsupported on this OS` when `host_os` excludes this host, including when the binary is absent, so an operator is not told to install it here. `not installed` is only for an executor this host may run whose command is missing. The full `orbit doctor` and the dashboard's Doctor panel agree: a routed crew whose executor excludes this host is `skipped` with the same `unsupported on this OS` message and no remediation.
- `orbit doctor providers` also reports tool telemetry per executor (`TOOLS` column; `tool_telemetry` in `--json` with `status`, `output_format`, `cause` and `remediation`), classified from the effective `args` and independent of whether the binary or sandbox is available. For `grok`, `--output-format streaming-json` is `supported`; `--output-format json` is `unavailable`: grok prints one result document with no tool-call frames, so runs still succeed and their answer and usage are read, but no tool calls or first-edit (turns-to-edit) samples are recorded, and the report gives the cause and the one-flag change to the installed `resources/executors/grok.yaml`. Any other, absent or non-grok configuration is `unknown`, never claimed to supply traces, and an executor with no `command` is `not_applicable`. The diagnostic is read-only and never rewrites an executor; only runs started after the operator edits it record tool calls.

## Executor authentication recovery probes

An executor may declare `spec.auth_probe`. Only a pull drain's **auth-excluded** providers run it: never at drain start, for healthy crews, after the window closes, or for capacity or refusal exclusions. The first attempt is due ten minutes after the failed leaf; failures wait twenty, then thirty minutes (the cap). Exclusions and probes are per provider account: a crew that sets [`env`](crews.md#per-crew-credentials-env) is excluded, probed and re-admitted alone, and the provider's default login covers its crews without `env`. A pass durably acknowledges those auth failures and re-admits that account's crews within the window; it never clears a preflight, capacity, crew or host exclusion. Without a probe the exclusion lasts the whole window: re-login and start a new drain. Owner-local drains use timed, task-specific [provider holds](crew-recovery.md#provider-failure-holds) instead. Drain-side view: [distributed drain runbook](../runbooks/distributed-drain.md).

Only the shipped **claude** executor declares one: a minimal `claude -p --model haiku` call with built-in and MCP tools disabled ([CLI contract](https://code.claude.com/docs/en/cli-reference)), a one-line prompt on stdin, and success on exit zero plus `ORBIT_AUTH_OK` on stdout. Each attempt costs one model request. `claude auth status` is not used: cached status can lag a real 401 and may refresh shared OAuth credentials. Codex, Gemini, Antigravity, Grok, Copilot, and Muse declare none (their minimal model-call and success contracts are not established); local-shell has no provider authentication.

Seeding preserves existing operator executor definitions; to opt in, add the declaration to the installed definition and start a drain:

```yaml
auth_probe:
  args: ["-p", "--model", "haiku", "--output-format", "text", "--tools", "", "--disallowedTools", "mcp__*", "--strict-mcp-config", "--mcp-config", '{"mcpServers":{}}', "--max-turns", "1", "--no-session-persistence"]
  stdin: "Reply with exactly ORBIT_AUTH_OK."
  timeout_seconds: 60
  success: { kind: stdout_contains, text: ORBIT_AUTH_OK }
  relogin_hint: "Run `claude auth login`, or renew `claude setup-token`."
```

- `args` replaces the activity argv and uses the same resolved executor command and launcher.
- `success.kind` is `exit_zero`, or `stdout_contains` with non-empty `text`; both require exit zero. `timeout_seconds` is 1–120.
- A probe gets the leaf runner's cleared environment plus `[execution.env].pass` (with a crew account's own [`env`](crews.md#per-crew-credentials-env), so it checks the credential that crew's leaves used), provider-required and fixed entries, sandbox resolution and provider carve-outs (the macOS login keychain included), bounded process-tree supervision, and no Orbit activity tool grants. Without explicit `allow_fallback`, a sandbox refusal keeps the exclusion and backoff.

`orbit doctor` (`provider-auth:<provider>`, or `provider-auth:<provider>/<crew>` for a crew account), `orbit run show <drain>` and the dashboard drain panel show provider account, host, exclusion time, auth class, re-login hint, credential origin and next probe time; doctor reads the record and calls no provider. Credential origin names a passed `CLAUDE_CODE_OAUTH_TOKEN` (or `ANTHROPIC_API_KEY`) by name only, else the macOS keychain or provider cache: the configured route, not a keychain read or proof of which credential the CLI used. To avoid sharing a rotating Desktop login, pass a dedicated `claude setup-token` credential as `CLAUDE_CODE_OAUTH_TOKEN`.

## Executor inspection tools (`inspection_args`)

An executor may declare `spec.inspection_args`: the arguments appended to its argv for a pinned-source inspection run (task pilot, escalation triage), so the CLI's own file-read and shell tools work unattended in the inspection checkout.

```yaml
spec:
  inspection_args: [--config, features.shell_tool=true, --config, features.unified_exec=true]
```

- An empty list means the executor's static `args` already enable those tools. The shipped **claude** executor declares `[]` (`--tools default` enables Read and Bash) and the shipped **codex** executor declares the shell and unified-exec features above. No other shipped executor declares the field.
- Omitting the field means the executor has no known inspection surface. Inspection draws skip its crews (see [System work draws from the complexity pools](crews.md#system-work-draws-from-the-complexity-pools)), and an explicit or run crew that names it fails before launch with the non-retryable `InspectionToolsUnavailable` error, which names the executor and this field. Declare the arguments once the CLI's native tools are known to work non-interactively under the `reviewer` profile.
- The activity input `image_input` is an object whose optional `images` lists the image paths as strings (`{"images": ["diagram.png"]}`); a null or absent `image_input` asks for no images, and an object without `images` or with an empty list asks for the canary alone. Any other shape — a non-object request, an unknown field, `images` that is not a list, a non-string or empty entry — fails before staging or launch with `image_input_unavailable` naming the invalid `image_input`; no requested image is dropped.
- An installed definition that omits the field takes the shipped default executor's declaration of the same name, so existing `codex` and `claude` installs inspect without reseeding. A value set on the installed definition wins.
- Read-only is enforced by the `reviewer` filesystem profile, not by these arguments.

## Executor image input (`image_input`)

An executor may declare `spec.image_input`: how image files reach its model. Images never travel in prompt text; they are staged as files in the run's scratch directory.

```yaml
spec:
  image_input:
    mode: attach_args          # or native_read
    formats: [png, jpeg, webp] # png is required: it is the canary's format
    max_images: 40             # canary included; at least 2
    attach_args: ["--image", "{path}"]   # attach_args only: argv per image
```

- `native_read`: the CLI's own file-read tool presents image files to the model. The prompt lists the staged paths and tells the agent to read each one.
- `attach_args`: the images are attached at launch, `attach_args` appended once per image with `{path}` replaced by the staged file. Only scratch paths reach argv, never content.
- A definition with an invalid declaration fails to load with `invalid spec.image_input: …`: an unknown key, no `png` in `formats`, a repeated format, `max_images` below 2, `attach_args` with `native_read`, `attach_args` mode without `attach_args`, or a template without `{path}`.
- Omitting the field means the executor cannot take images. An activity whose input asks for images (`image_input`) on such a crew, or on a crew that sets [`vision = false`](crews.md#crewsname--which-provider-model-runs-the-task), fails before launch with the non-retryable `image_input_unavailable` error, which names the executor and `spec.image_input`.
- An installed definition that omits the field takes the shipped default executor's declaration of the same name, as `inspection_args` does. A value set on the installed definition wins.
- Shipped declarations: **claude** `native_read` (PNG, JPEG, WebP, GIF), **codex** `attach_args` with `--image {path}` (PNG, JPEG, WebP) and **muse** `attach_args` with `--image {path}` (PNG). Each is declared only because its lane passed the canary probe below. The gemini, antigravity, grok and copilot executors declare none.
- Only regular files are staged. A requested image that is a FIFO, socket, directory, device or another non-regular file — including a symlink that resolves to one — is refused before launch with a `stage image …` error that names the path. When the source opens, the refusal reports `not a regular file (found <kind>)`; when opening the source itself fails — as it does for a Unix-domain socket on Linux, refused with its OS error such as `No such device or address (os error 6)` — the staging error retains that open error instead. The source is opened without blocking, so a named pipe with no writer cannot stall preparation before provider supervision starts.

**The canary.** Every image-input invocation is first given a canary: a generated PNG of a random six-character code, created per invocation in scratch and never written to a task. The agent must return the code as `result.canary`. A delivery result without it, or with another code, is refused as `[image_not_seen]`, a retryable step failure. The image-input probe records the same miss for that crew and the probe run still completes, so one crew does not fail the probe batch. Each check emits an `activity.image_canary_checked` audit event with the crew, mode and `seen`. The canary proves delivery, not judgement.

**Visual hold.** A crew whose newest two or more canary checks all missed is reported as held until 30 minutes after its last miss, doubling with each further consecutive miss up to 24 hours, like a [provider failure hold](crew-recovery.md#provider-failure-holds). Its next passing check clears it. The hold is read from the audit events, so it covers probes and deliveries alike.

**Probing.** `orbit agents vision-probe` runs the canary for every enabled crew outside any delivery (one `image_input_probe_pipeline` run each) and prints one row per crew: executor, model, mode, result and any visual hold. Results are `seen`, `image_not_seen`, `no_image_input`, `vision_off`, `no_cli_runtime`, `failed` or `timeout`. A crew that finishes without the code is `image_not_seen` on its own row, and that probe run still completes. Each row uses only that run’s canary check. If a settled run’s check cannot be found, its row is `failed` with an explanatory detail, even if the run succeeded; another run’s check cannot supply the result. `--crew <name>` (repeatable) probes only those crews, and fails before launching anything when a named crew cannot take images. `--timeout-seconds` bounds the wait (default 900). Declare `image_input` on an executor only after its probe passes, and set `vision = false` on a crew whose model keeps returning `image_not_seen` on a lane that passes for others.
