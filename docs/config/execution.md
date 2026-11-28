# Execution configuration

## Execution

| Key | Default | What it does |
|---|---|---|
| `execution.codex.sandbox` | `workspace-write` | Codex sandbox mode: `read-only`, `workspace-write` or `danger-full-access`. Under `workspace-write`, Codex's extra writable roots are Orbit's runtime stores, never the whole workspace `.orbit` or `~/.orbit`. `orbit init` seeds `danger-full-access` globally. [Security key](README.md#where-config-lives). |
| `execution.codex.approval_policy` | unset | **Advanced.** `untrusted`, `on-request` or `never`. Security key. |
| `execution.env.pass` | `HOME`, `PATH`, `CODEX_HOME`, `TMPDIR`, `USER` (+ `__CF_USER_TEXT_ENCODING` on macOS) | Environment names passed into every crew's agent subprocesses; a crew's [`env`](crews.md#per-crew-credentials-env) adds names for that crew alone. Security key. See [`[execution.env]`](#executionenv--the-agent-subprocess-environment). |
| `execution.env.login_auth` | `[]` | **Advanced.** Providers (`claude`, `codex`) whose agents authenticate through the CLI's own login. Unset names from `pass` that belong to a listed provider are not reported. See [`[execution.env]`](#executionenv--the-agent-subprocess-environment). |

One `proc.spawn` call inside a managed activity may use a timeout of at most 45 minutes, also capped at the activity's remaining wall-clock budget (passed to the agent as `ORBIT_ACTIVITY_DEADLINE_UNIX_MS`); outside an activity the ceiling is 60 seconds, and the result reports `timeout_ceiling_ms` and its source. A provider process that produces no stdout or stderr and uses no CPU for 30 minutes is ended and its step fails as stalled, so the usual retry and recovery path runs; the wall-clock timeout still applies. Both are fixed [ORB-22808]: `execution.proc_spawn_max_timeout_minutes` and `execution.agent_stall_minutes` are [deprecated](validation.md#validation-and-errors).

## `[execution.env]` — the agent subprocess environment

Every agent subprocess (bare, Bubblewrap or `sandbox-exec`) starts from a **cleared** environment and receives the following groups plus Orbit's managed Python bytecode default:

| Group | Contents |
|---|---|
| Baseline | `HOME`, `LANG`, `LC_ALL`, `LOGNAME`, `PATH`, `SHELL`, `TERM`, `TMPDIR`, `TZ`, `USER`. `PATH` starts with any [`workflow.validation_env.path`](workflow-validation.md#agent-sessions-share-the-configured-path) entries. |
| `pass` | Names listed in `execution.env.pass`, except the reserved task-identity names below. Default: `HOME`, `PATH`, `CODEX_HOME`, `TMPDIR`, `USER`, plus `__CF_USER_TEXT_ENCODING` on macOS. |
| Provider extras | Variables the selected provider runtime declares it needs. |
| Orbit envelope | Named `ORBIT_*` variables: run, task and session identity (`ORBIT_RUN_ID`, `ORBIT_TASK_ID`, `ORBIT_SESSION_ID`, …), locators (`ORBIT_WORKSPACE`, `ORBIT_WORKTREE_ROOT`, `ORBIT_BIN`, `ORBIT_REGISTRY_ROOT`, …) and activity bindings (`ORBIT_ACTIVITY_*`, `ORBIT_STEP_INDEX`, …). Privilege-bearing names (`ORBIT_OPERATOR`, `ORBIT_WORKSPACE_CLAIM_TOKEN`) are not admitted; an inherited `ORBIT_ROOT` is removed. |
| Crew `env` | The global `[crews.<name>] env` entries for the crew the agent runs as are applied after `pass` (except the reserved task-identity names below). Each entry's name takes its source's value from the launching process and replaces the same name from `pass`. Other crews never receive it. An unset source leaves the name out for that crew. See [per-crew credentials](crews.md#per-crew-credentials-env). |
| Orbit-managed defaults | `PYTHONDONTWRITEBYTECODE=1` and, for provider sessions, `PYTHONPYCACHEPREFIX` pointing to private invocation scratch. Nonempty values already present in the agent environment are preserved. |

This is an allowlist, not a secret filter: a benignly named credential such as `DATABASE_URL` never reaches an agent unless you name it in `pass` or in a crew's `env`. Agents keep network access, so this is the boundary against accidental exfiltration.

```toml
[execution.env]
pass = ["HOME", "PATH", "CODEX_HOME", "TMPDIR", "USER", "GITHUB_TOKEN"]
```

- `pass` replaces the default rather than extending it; restate the baseline names you want.
- `pass` is shared by every crew. A crew's [`env`](crews.md#per-crew-credentials-env) entry adds a name for that crew alone and replaces the same name from `pass` for it.
- A listed name that is unset is absent, not empty. Names must be valid identifiers.
- Task identity is reserved. CLI dispatch clears `ORBIT_TASK_ID`, `ORBIT_ACTIVE_TASK_ID` and `AGENT_TASK` from every provider environment, including values inherited from the launching process, listed in `pass` or mapped by a crew's `env`. An activity bound to one task then sets all three to that task's ID; an unbound or multi-task activity receives none.
- `pass` is a [security key](README.md#where-config-lives): a workspace file that omits it gets the built-in default, not the global value.
- `login_auth` lists providers whose CLI keeps its own login (`claude login`, `codex login`), such as `login_auth = ["claude"]`. For a listed provider, `orbit doctor`'s `env-pass` row, the drain, ship and job start warnings and `orbit attention` skip its credential variables in `pass` (`claude`: `CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`; `codex`: `CODEX_API_KEY`) when unset. It is a declaration, not a check: Orbit reads no credential, so a logged-out provider is not detected. The operator sets it on each host that authenticates this way. With it unset, or not naming the provider, those names warn as usual. It only mutes the unset-name warning; it does not change what agents receive.
- Inheriting the full environment is not configurable; a stale `execution.env.inherit` is ignored. The internal inherit seam still removes privilege-bearing `ORBIT_*` names (`ORBIT_OPERATOR`, `ORBIT_WORKSPACE_CLAIM_TOKEN`, and any other `ORBIT_*` name outside the execution envelope) before a child starts.

**Unset names.** A `pass` name the launching process lacks reaches no agent unless a timer-started run reads it from `~/.orbit/clock.env`, and the provider may use another login; this warning alone does not force API-key authentication or refuse the start. Start drains from a login shell, or export the variable in the service's environment. A drain or ship start (`orbit run auto`, `--pull` included, and `orbit run ship`; MCP `orbit.workflow` action `auto` start and `orbit.workflow` action `ship`; the dashboard) reports them in its `warning` (a `Warning:` line in CLI text), and `orbit run job` warns once on stderr, for each unset or empty name you added; these starts judge only the launching environment, so a name `~/.orbit/clock.env` supplies still warns there. All of them record the names on the run (`Unset env:` in `orbit run show`, `run.env_pass_unset` in JSON, the dashboard run detail). Built-in defaults are not reported (`CODEX_HOME` and macOS's `__CF_USER_TEXT_ENCODING` can legitimately be absent). `orbit doctor`'s `env-pass` row counts a `pass` name as held when its own environment or a non-empty `~/.orbit/clock.env` value supplies it, so it can report `ok` where a manual start from that shell still warns. It checks [crew sources](crews.md#per-crew-credentials-env) against the shell it runs in, which `clock.env` does not clear, and against installed managed services, where the sweep service also counts `clock.env`. These are warnings only; the start proceeds and no value is printed. A missing per-crew source instead excludes that crew and refuses its activities; see [per-crew credentials](crews.md#per-crew-credentials-env). The separate [Claude-on-macOS check](#executionenv--the-agent-subprocess-environment) below can also refuse an activity without a credential.

### Provider credentials

Provider credentials are opt-in: neither the shipped config nor the built-in
`pass` default includes `CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY` or
`CODEX_API_KEY`. Add only the names needed for the authentication route you
intend to use.

| Authentication route | Agent variable |
|---|---|
| Claude dedicated worker token (`claude setup-token`) | `CLAUDE_CODE_OAUTH_TOKEN` |
| Claude API key | `ANTHROPIC_API_KEY` |
| Codex noninteractive API-key override (`codex exec`) | `CODEX_API_KEY` |

For Codex, `CODEX_API_KEY` is the documented per-run API-key override;
`OPENAI_API_KEY` alone is not that override. See
[OpenAI's noninteractive authentication guidance](https://developers.openai.com/codex/noninteractive#use-api-key-auth).
To share the key through `pass`, add `CODEX_API_KEY` to the effective list,
preserving existing entries. That list replaces the default and applies to
every crew. A workspace file that omits it gets the built-in default rather
than the global list.

To use the key for one crew, add this to the existing crew in global config:

```toml
# ~/.orbit/config.toml
[crews.sol]
env = ["CODEX_API_KEY=CODEX_CREDITS_API_KEY_1"]
```

The right-hand side is a variable name whose value comes from the launching
process, not a literal key. Neither name needs adding to `pass`. Other crews
do not receive this mapping. Use an owner-only [`file:` source](crews.md#per-crew-credentials-env)
when the credential should be read directly from a file at agent start.

Check `orbit config get execution.env.pass`, `orbit config get crews.sol.env`
and `orbit doctor --check env-pass --json` on the execution host. A successful
SSH login-shell check does not prove the web service holds the source. Even
when the service starts through `bash -lc`, later profile edits require a
service restart before new runs inherit them. Inspect the effective unit and
start time, then verify names only in the live service or worker; never print
key values. Codex's stored `login status` alone does not prove which credential
a worker uses with a per-run override.

**Worker scope.** The same admitted names are given to a detached pipeline worker. On Linux, containment forwards them with name-only `systemd-run --setenv=NAME`, so the values stay in the child environment and never appear in `systemd-run` or worker argv. An explicit worker edit, such as a toolchain `PATH` or removing `ORBIT_ROOT`, wins over the admitted snapshot. See [worker containment](machine.md#worker-containment).

**Clock-started runs.** launchd (macOS) or a systemd user timer (Linux) starts `orbit clock tick` with no login environment. The tick reads `~/.orbit/clock.env` (created comment-only, mode `0600`, by `orbit routine init --install-clock`) and gives each discovered workspace's runs the names that workspace's effective `pass` admits, and the sources its crews' [`env`](crews.md#per-crew-credentials-env) entries read, as child-environment defaults; the tick's own process environment is never changed. A non-empty value already in the tick's environment wins, and an empty one can be filled from the file. A dry run loads nothing. Unit files never contain a secret. A `clock.env` that is a symlink, owned by another user, or group- or world-readable is refused as a load error (the tick still runs). `orbit clock tick --json` reports loaded names as `clock_env_loaded`, never values. Sandboxed agents can't read `clock.env` itself ([SECURITY.md](../../SECURITY.md#sandbox-model-and-known-limits)).

**Claude on macOS.** A Claude activity with neither `CLAUDE_CODE_OAUTH_TOKEN` nor `ANTHROPIC_API_KEY` in its environment is refused before `claude` starts, rather than fall back to the Claude Desktop login, which the Desktop revokes mid-run. The check reads the crew's composed environment, so a crew whose [`env`](crews.md#per-crew-credentials-env) maps its own `ANTHROPIC_API_KEY` runs without the shared token. A drain submitted on such a host (local or `--pull`) excludes the Claude crews that would be refused from its crew window instead, so it claims only work it can run, and its submission names the remediation once; an explicit `--allow-crew` that names only such crews is refused. `orbit doctor`'s `claude-worker-token` row warns ahead of time, naming the `pass` list or `clock.env` that lacks the token, or a crew credential source `clock.env` lacks; a `clock.env` token under a name `pass` omits counts as lacking.

---
