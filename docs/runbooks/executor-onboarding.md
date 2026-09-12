---
type: runbook
summary: Add and validate a CLI-agent or deterministic local-shell executor without changing existing users' routing or state.
last_validated: 2026-09-12
tags: [contributors, executors, providers, testing]
paths: ["crates/orbit-agent/**", "crates/orbit-core/assets/executors/**", "crates/orbit-core/src/application/executor.rs", "crates/orbit-core/src/adapter/engine_host/v2_host/cli_executor.rs", "crates/orbit-engine/src/activity_job/**"]
related_features: [activity-job, policy-sandbox]
related_artifacts: [ORB-11294, ORB-11295, ORB-11296, ORB-11299]
---

# Onboard an Executor

Use this runbook when adding a supported execution lane to Orbit, or when adding a deterministic local command executor. It is a contributor procedure: configuring an already shipped provider is covered by [CONFIG.md](../CONFIG.md).

## Before changing anything

Start from a disposable checkout and registry. Do not point fixtures at a developer's `~/.orbit`, a live workspace, a logged-in provider directory, a production task store, or a real routing configuration. Fixture tests should construct an in-memory `OrbitRuntime`, use a temporary worktree and audit directory, and substitute a fake CLI through the executor definition (as the Pi and Antigravity integration tests do). A fake must record argv, stdin, cwd, and any edit so the contract is observable without credentials or network access.

First establish the installed and source contracts separately:

```bash
orbit --version
orbit executor list --json
orbit executor show <existing-executor> --json
<provider-cli> --help
<provider-cli> --version
```

Use the provider's published CLI help and a pinned observed version to establish flags, stdin format, terminal output, authentication behavior, and supported model/effort values. Do not copy flags from another provider and do not claim a universal `--json`, sandbox, login, or MCP flag. `docs/CONFIG.md` records the currently verified contracts for shipped lanes; the Pi, Antigravity, and OpenCode entries are representative, not templates with interchangeable flags.

Verify the prompt transport specifically, and do not assume a CLI that documents a positional prompt cannot accept stdin. OpenCode documents `opencode run [message..]`, but upstream reads piped stdin when stdin is not a TTY and uses it as the whole message when the positional is empty — which is what let that lane keep the envelope off argv. If the published help is silent, read the upstream argument handling before choosing a transport. [ORB-11295]

An authenticated smoke test is optional and needs explicit operator consent. Run it only against an isolated test workspace and test account or disposable credentials. State exactly what it did not prove (for example, no authenticated account, no vendor model access, or no OS sandbox available); passing fixture contract tests remains sufficient for a normal source change when they exercise Orbit's real dispatch seam.

## Choose the execution family

This choice determines the whole integration. Do not revive the deleted v1 executor registry or make a YAML definition alone and call it an implementation.

| Need | Use | It carries | It must not carry |
| --- | --- | --- | --- |
| An interactive-capable coding-agent CLI that receives an Orbit task envelope | `direct_agent` / `agent_cli` through the v2 CLI-agent path | provider identity, model and effort where supported, prompt bytes on stdin, tool authority, response envelope, provider state grant | shell command data from task input |
| A deterministic program or script step | `local_shell` deterministic action | static `command` + `args` or explicit `shell` + `script`, fixed env, cwd, timeout, exit/output report | model, prompt, agent tool authority, registry/workspace identity variables |

`local_shell` is the supported v2 successor to the old `cli_command` spelling. Its parser accepts that legacy spelling for existing definitions, but new assets use `local_shell`. It shares process supervision and optional sandbox selection with the agent runner; it is not an agent adapter. See [the local-shell reference](../design/executors/specs/local-shell.md) for the activity shape.

## Source and symbol map

Verify the seams against the current checkout before editing. These are the authoritative extension points, not historical design records.

| Concern | Current source / symbol | Required change or check |
| --- | --- | --- |
| Canonical provider identity and capabilities | `crates/orbit-types/src/workflow/activity_job/mod.rs` — `Provider`, `ProviderEntryPoint` | Add a canonical identity and capability only for a new agent lane; keep the lane identity separate from the model vendor. |
| Crew configuration, detection, and migration | `crates/orbit-config/src/` and `crates/orbit-cli/src/command/init/` | Add detection and config validation/migration only where current code routes provider identities. Preserve existing explicit crew model pins and custom definitions. |
| Shipped executor catalog and safe seed migration | `crates/orbit-core/assets/executors/<name>.yaml`; `crates/orbit-core/src/application/executor.rs` — `DEFAULT_EXECUTOR_FILES`, `seed_default_executors_for_platform`, `migrated_default_executor_for_platform` | Register the embedded asset and make seeded-default migration narrow: never overwrite a customized command, credentials, or asset merely because a new default exists. |
| Executor lookup and the family boundary | `crates/orbit-core/src/adapter/engine_host/v2_host/cli_executor.rs` — `resolve_cli_executor`, `resolve_local_shell_executor` | Preserve the hard split: agent resolution accepts only `direct_agent` / `agent_cli`; shell resolution accepts only `local_shell`. |
| Provider argv, prompt bytes, and model/effort adapter | `crates/orbit-agent/src/providers/<provider>/`; Pi: `PiCliTransport::args`, `PiCliTransport::stdin`; Antigravity: `AntigravityCliTransport::args`, `AntigravityCliTransport::stdin` | Add a provider-specific adapter. Render only documented flags; send prompt/envelope bytes on stdin, never argv. |
| Provider terminal-output reduction | `crates/orbit-agent/src/providers/<provider>/<provider>_output.rs` | Reduce the provider protocol to trustworthy terminal response bytes before the common completion-envelope parser. Reject stale, partial, malformed, or failed frames. |
| Child lifecycle, cancellation, and cleanup | `crates/orbit-engine/src/activity_job/cli_runner/`; `crates/orbit-exec/src/supervision/` | Reuse the v2 runner and supervisor. Do not add provider-owned process spawning or cleanup. |
| Deterministic shell input and output | `crates/orbit-engine/src/executor/automation/shell.rs` — `local_shell`, `parse_shell_config`, `compose_argv` | Keep argv in static activity config. Run an actual shell only when `shell` and `script` are declared explicitly. |
| OS sandbox / provider home grant | `crates/orbit-core/src/adapter/engine_host/v2_host/sandbox.rs`; `crates/orbit-exec/src/macos_sandbox/provider_dirs.rs`; `crates/orbit-exec/src/macos_sandbox/compile.rs` | Give only the active provider's required state directory a write grant. Keep the activity `fsProfile` authoritative for the worktree. On macOS, a state-directory grant is not a login: Claude, Copilot, and Cursor keep their default login in the user keychain and receive a narrow `$HOME/Library/Keychains` read carve-out. |
| End-to-end fixtures | `crates/orbit-core/tests/pi_fake_agent.rs`, `crates/orbit-core/tests/antigravity_fake_agent.rs`, `crates/orbit-core/tests/opencode_fake_agent.rs`, `crates/orbit-engine/tests/v2_local_shell.rs` | Exercise the real v2 dispatch seam with fakes; add output/error/timeout/cancellation cases. |

## Add a CLI-agent executor

1. Confirm that the provider's local CLI is an agent capable of completing one non-interactive turn. Record its actual version, official help/output contract, login-state directory, prompt transport, terminal response shape, and supported model/effort vocabulary in `docs/CONFIG.md`.
2. Add the canonical provider identity at the shared identity boundary if it does not already exist. A model such as `openai/gpt-4o` routed through Pi remains a `pi` execution lane: its login, state directory, sandbox grant, audit attribution, and unsupported-capability error stay Pi's. Never infer the Orbit provider from a model string.
3. Implement the provider runtime under `crates/orbit-agent/src/providers/<provider>/`. Follow the Pi or Antigravity split: a small argv/stdin transport, a runtime factory with only non-secret required environment, and an output normalizer. Pass optional model/effort only when the provider contract supports them; reject unsupported combinations during configuration resolution rather than silently dropping or remapping them.
4. Add the bundled executor asset in `crates/orbit-core/assets/executors/<provider>.yaml` and register it in `DEFAULT_EXECUTOR_FILES`. Use static headless flags only. Keep the execution envelope and prompt off argv so process listings, audit argv, and spawn errors cannot expose task content or secrets.
5. Wire provider discovery and init seeding through the current configuration/init seams. `orbit init` must add a crew only when the local CLI is detected; it must not replace a user-authored crew, credential, command override, or model pin. Check both a fresh seed and a legacy/customized asset through the migration path.
6. Choose the sandbox posture deliberately. Shipped CLI agent assets opt in to the OS sandbox marker that becomes macOS `sandbox-exec` or Linux Bubblewrap when supported. The provider's own approval flag cannot grant filesystem access the enclosing sandbox denies. Do not use a provider's native sandbox flag as a substitute for Orbit's boundary. If the CLI's default login lives in the macOS login keychain, add it to `provider_reads_macos_login_keychain` and teach `macos_keychain_auth_diagnostic` that CLI's auth-failure wording; a state-directory write grant does not make the keychain readable.
7. State MCP and shell-tool reach precisely. If the provider has no MCP client, do not add one by implication: the execution envelope must direct the agent to the `orbit` binary supplied on its `PATH`, and tests must retain whatever provider shell capability this route needs. Tool grants and caller-role gates remain enforced by `orbit tool run`.

### Worked configuration: Pi

This existing lane demonstrates the separation between executor identity and inference model. It is a configuration example, not an instruction to install or log into Pi during development.

```toml
[crews.pi]
provider = "pi"
model = "openai/gpt-4o"
effort = "high"
description = "Pi through its local CLI"
tags = ["implementation"]
```

The crew chooses the `pi` executor. Orbit renders Pi's documented `--model openai/gpt-4o` and `--thinking high`, but the run remains `pi` for its agent directory, authentication, audit and sandbox policy. The shipped asset provides Pi's static non-interactive flags; `PiCliTransport` supplies only per-run argv and stdin. Before changing this or adapting it to another CLI, inspect the effective installed definition rather than assuming a release's asset matches the checkout:

```bash
orbit config show
orbit executor show pi --json
pi --version
pi --list-models
```

Do not put an API key in the executor asset or argv. An operator who deliberately uses an API key adds its variable name to `[execution.env].pass`; an interactive provider login is a separate, unsandboxed setup action and is never performed by an Orbit workflow turn.

### macOS login-keychain credentials

On macOS, Orbit's `macos-sandbox-exec` profile denies `$HOME/Library/Keychains` by default, then re-allows it only for the confined provider when that provider's CLI stores its login there. `/Library/Keychains` and `/System/Library/Keychains` stay denied. An activity `denyRead` on the user keychain directory outranks the carve-out.

| Provider | Default macOS login store | How to skip the keychain |
| --- | --- | --- |
| `claude` | login keychain item `Claude Code-credentials` | `ANTHROPIC_API_KEY` via `[execution.env].pass` |
| `copilot` | login keychain item `github-copilot-app` | `COPILOT_GITHUB_TOKEN` via `[execution.env].pass` |
| `cursor` | login keychain items `cursor-access-token` / `cursor-refresh-token` | `CURSOR_API_KEY` via `[execution.env].pass`, or log in with `AGENT_CLI_CREDENTIAL_STORE=file` and pass that variable too (it writes `$HOME/.cursor/auth.json`, the CLI re-reads the variable on every run, and setting it later does not migrate an existing keychain login) |

A failed keychain-backed step records Orbit's diagnosis — sandbox hid the item versus a real logout — on the run error and the task's `workflow_run_failed` note, not only `exited with code Some(1)`. Do not document a provider as file-backed on macOS from its state directory alone; inspect the CLI's credential store.

### Copilot model pins

Copilot's model catalog depends on the authenticated account and can change
between CLI releases. Start `copilot` and enter `/model` to list the exact ids
available to that account. Re-run this check after every Copilot CLI upgrade
before retaining or changing a `crews.<name>.model` pin; a rejected pin fails
closed rather than falling back to the CLI's ambient model choice.

## Add a deterministic local-shell executor

Use a `local_shell` activity only for a bounded, reproducible command. Its `config` is the authority for `command`/`args` or for `shell`/`script`; rendered job input must never become child argv. The shared `local-shell` definition may supply a static command prefix, environment, and fallback timeout, but a shell action gets no agent prompt, model, tool allowlist, or workspace/registry identity variables.

For a new deterministic default, add a `kind: Executor` resource with `executor_type: local_shell`, then test the actual activity through `local_shell`. Preserve the existing behavior that a bare definition runs under the shared supervisor and cwd containment checks; adding an OS sandbox requires both an explicit sandbox declaration and an activity `fsProfile`. Report the effective sandbox in output. Use explicit `/bin/sh` plus `script` only when shell semantics are really required; otherwise use literal argv.

## Output, errors, and lifecycle contract

An agent exit status and its response envelope are independent evidence; both must be valid. The v2 path fails closed when the CLI exits non-zero even if stdout contains success-looking JSON. It also fails closed on missing terminal output, malformed protocol records, an error/aborted terminal event, missing response/completion envelope, and stale prompt echo. Provider normalization must retain only the documented terminal answer before the common envelope reader scans it. Pi's `message_end` reducer exists specifically to avoid accepting the later conversation replay in `agent_end`; Antigravity accepts only a terminal `result` whose status is `SUCCESS`; OpenCode keeps only assistant `text` parts, because its `tool_use` frames carry tool output that can replay Orbit's own prompt and its `reasoning` frames can carry a draft envelope.

Set a wall-clock deadline through the agent activity or `local_shell` config and reuse the supervisor. Verify timeout cancellation and child-process cleanup rather than implementing a provider-specific timer. A normal local-shell non-zero exit is an activity failure unless `allow_nonzero_exit` is explicitly true; when true, its structured output still reports `success`, `exit_code`, `stderr`, `timed_out`, `timeout_ms`, `argv`, `cwd`, and `sandbox`.

## Validation matrix

| Case | Fixture assertion |
| --- | --- |
| Discovery and seeding | Detected CLI creates the intended crew/executor; missing CLI produces the stable unavailable error; an existing custom definition and model pin survive seeding/sync. |
| Identity and configuration | Canonical provider is retained when the model names another vendor; aliases/migrations are explicit; invalid or unsupported model/effort fails instead of falling back. |
| argv, stdin, and cwd | Exact static and per-run argv; prompt/envelope bytes appear only on stdin; cwd is the intended temporary workspace; secrets are absent from argv, result, and diagnostics. |
| Normal output and usage | Only the documented terminal frame reaches the completion parser; a successful envelope projects its result; usage is propagated only when the provider's retained terminal payload supports it. |
| Error evidence | Non-zero exit, zero-exit provider error, malformed JSON/JSONL, incomplete/stale terminal data, no terminal frame, and absent response/completion envelope each fail closed. |
| Time and process lifecycle | Deadline returns a timeout failure, cancellation terminates the child process group, and no child remains after a test. |
| Sandbox and state isolation | Fixture needs no live provider state; only the active provider state root is writable; activity profile restricts the disposable worktree; a shell executor has no agent-only authority. |
| MCP / CLI envelope | Native-MCP lanes receive only their supported configuration; non-MCP lanes reach granted Orbit tools through the injected `orbit` binary and fail predictably when their required shell tool is unavailable. |
| Local-shell injection resistance | Runtime input cannot modify argv; both literal argv and explicit shell forms behave as declared; non-zero and timeout output is audited. |

Run focused tests during development, replacing `<provider>` only with a real crate/package/test target:

```bash
cargo test -p orbit-agent <provider>
cargo test -p orbit-core --test <provider>_fake_agent
cargo test -p orbit-engine --test v2_local_shell
./scripts/generate-doc-indexes.sh --check
./scripts/sync-plugin-skills.sh --check
make ci-fast
make ci-lint
make goldens
```

The first two commands are examples of target selection, not provider CLI flags. If no provider-specific test target exists yet, run the package's relevant test module and add the integration fixture before calling the lane supported. `make ci-fast`, `make ci-lint`, and `make goldens` are the repository handoff gates; do not substitute a live authenticated smoke for fixture coverage.

## Package and hand off managed assets

`crates/orbit-core/assets/` is the canonical source for embedded executors and skills. A new executor asset needs the catalog registration in `DEFAULT_EXECUTOR_FILES`; a changed skill reference needs the matching `include_str!` registration in `crates/orbit-core/src/application/skill.rs::DEFAULT_SKILL_FILES` only when it adds a new file. Then materialize the committed plugin mirror from canonical assets:

```bash
./scripts/sync-plugin-skills.sh
./scripts/sync-plugin-skills.sh --check
./scripts/generate-doc-indexes.sh
./scripts/generate-doc-indexes.sh --check
```

Do not edit `plugin/skills/` as an independent source. The sync script copies each canonical skill tree, and `make ci-fast` rejects drift. Do not create or edit the runtime `.orbit-managed-assets.json` manifest: reconciliation owns its digests and records the new catalog after `orbit workspace sync`. Run the managed-asset migration/sync tests when changing executor seeding; use `orbit workspace sync --check` to inspect an installed workspace before asking an operator to reconcile it. Customized managed assets must be preserved with a warning or migration path, never overwritten silently.

## Escalate instead of guessing

Stop and seek a scoped design/task decision when the provider requires a new cross-crate dependency, a new execution mode, a network service, a credential transport outside the cleared environment policy, an unsupported platform boundary, or a generic protocol abstraction without a second concrete use. Record a follow-up rather than smuggling a broader runtime into an executor patch.

## Related references

- [Orbit Configuration](../CONFIG.md) — current crews, provider identity, configuration inheritance, and shipped provider contracts.
- [Local-shell executor reference](../design/executors/specs/local-shell.md) — deterministic activity schema and output shape.
- [Linux sandbox runbook](linux-sandbox.md) — host prerequisite for Linux CLI-agent dispatch.
- [Runbook conventions](CONVENTIONS.md) — maintenance and index generation rules.
