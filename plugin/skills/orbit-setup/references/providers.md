# Providers and CLI setup

## Provider identity and resolution

Choose the execution lane first. Canonical provider IDs are `claude`, `codex`,
`gemini`, `grok`, `copilot`, `antigravity`, `muse`,
`ollama` and `openai_compat`. The last two are recognized but unsupported at the
Orbit CLI entry point; `openai-compat` is an accepted spelling of `openai_compat`.
Case and surrounding whitespace are ignored. Deprecated aliases resolve with
an `orbit.config.crew` warning: `anthropic` → `claude`, `openai`/`chatgpt` →
`codex`, `google` → `gemini`, `xai` → `grok`. The warning says the provider
"uses a deprecated alias; resolving to the canonical id — update the config".
Replace aliases with canonical IDs.
The newer lanes have no aliases. A model vendor routed through a lane never
changes its provider: a Claude model through Copilot is still `copilot`.

**Retired providers.** The `cursor`, `pi` and `opencode` lanes were retired. A crew naming one fails to load, naming the crew, the retired provider and the remedy: pick a supported provider (`claude`, `codex`, `gemini`, `grok`, `copilot`, `antigravity`, `muse`). `orbit doctor` reports the same refusal. Persisted records naming them still read and display. The Cursor editor MCP client (`orbit mcp init --cursor`) is a separate surface and is unaffected.

For task dispatch, the first applicable tier wins:

1. Explicit run `--crew` or run-input `crew`.
2. Task `crew`, including a still-valid pool or purpose assignment. Admission
   redraws stale automatic assignments as described in [crew selection](configuration.md#crew-selection-and-actual-execution).
3. `workflow.default_crew` (workspace config over global config).
4. `CONSTELLATION_DEFAULT_PROVIDER`: a provider ID or alias; `claude` selects
   `opus`, `codex` selects `sol` when defined, other IDs select a same-named crew.
   This applies even without config files; unset/blank leaves the built-in default.
5. Built-in default: `opus`, or the legacy `claude` crew.

Inspect `orbit config show` for the effective default and its source, task
assignment history for pool provenance, and `orbit run show <run>` for the
recorded crew. A configured default outranks the environment variable; otherwise
an invalid environment provider is refused at config admission.

An explicit lane that cannot run never silently switches runtime. Refusals include
`provider openai_compat is unsupported by the Orbit CLI entry point (HTTP-only)`,
`provider ollama is unsupported by the Orbit CLI entry point`, and
`unknown provider '<x>'; expected one of` followed by the canonical list.
Correct the provider ID or choose a supported lane. A missing binary fails
permanently naming that binary: install it on the execution host and fix `PATH`.
An undefined default fails config load; a disabled selected crew refuses dispatch
naming the crew: correct the name or deliberately enable it, then recheck.

## Provider setup

1. On the execution host, choose the lane above and follow its CLI notes below.
   Install only the requested CLI, authenticate interactively outside Orbit,
   and list its current model IDs. CLI installers, auth stores and model catalogs
   depend on the CLI version; check its help after upgrades rather than copying
   an old model pin. Orbit does not perform login for an unattended run.
2. Inspect `orbit config get execution.env.pass`. Preserve existing needed names
   when adding the credential or custom-home names below; `pass` replaces the
   array. It lists names, never secret values. Credentials are not inherited
   automatically; a workspace policy must restate this allowlist.
3. Run `orbit doctor providers --format table` and read the selected row:
   executor/type identifies the lane, CLI names its binary, FOUND reports
   installation, SANDBOX names the configured boundary, READY reports sandbox
   readiness (on Linux), BWRAP names the selected Bubblewrap source and version
   (`-` when the sandbox is not linux-bwrap), and LAUNCHER shows the resolved
   CLI path. LIMITS reports usage readings. Neither FOUND nor READY proves
   authentication or tool access.
   Fix a missing CLI or sandbox first; see [Linux sandbox](linux-sandbox.md).
4. Add or update a named crew in the chosen config file, using the model ID
   returned by that CLI. For example, replace the model placeholder before saving:

   ```toml
   [crews.copilot]
   provider = "copilot"
   model = "<id from the CLI model list>"
   enabled = true
   ```

   Set it as default with `orbit config set workflow.default_crew copilot` (add
   `--global` for a user-wide default). Check `orbit config show`; a task pin or
   pool assignment can still outrank it. Changing the default does not dispatch work.
5. Register MCP only for lanes that support it below. In the intended agent
   client, discover the intended workspace and exercise an audited read such as
   `orbit.task.show` on an existing task. Shell lanes use
   `orbit tool run orbit.task.show --input '{"id":"<existing-task>"}'` with the
   inherited agent identity. Verify the returned workspace/task and audit record.
   Use a disposable fixture for any execution check; dispatch real work only when authorized.

### Copilot

Install `npm install -g @github/copilot`; verify `copilot --version`. The retired
`gh copilot` extension is not this lane. In interactive `copilot`, use `/login`
and `/model` to list IDs; Orbit passes the crew model explicitly, overriding
`COPILOT_MODEL` and a saved selection. Token precedence is `COPILOT_GITHUB_TOKEN`,
`GH_TOKEN`, `GITHUB_TOKEN`, then saved login: admit the chosen token name in
`execution.env.pass`. Preserve `COPILOT_HOME`/`XDG_CACHE_HOME` when customized.
Writes cover `COPILOT_HOME` (else `~/.copilot`) and `$XDG_CACHE_HOME/copilot`
(else `~/.cache/copilot`); macOS reads
`~/Library/Keychains`, with no `~/.config/gh` grant. Copilot has a native MCP
client (`~/.copilot/mcp-config.json`); Orbit init has no Copilot target, so
configure the client explicitly or use its shell and `orbit tool run`.
`Error: Authentication failed` can mean missing Copilot entitlement; a
`session.warning` with `warningType: "policy"` can mean org-disabled third-party
MCP. Check login/entitlement or ask the org admin to fix policy.

### Antigravity

Install `curl -fsSL https://antigravity.google/cli/install.sh | bash` per the
[CLI installer](https://antigravity.google/docs/getting-started/),
verify `agy --version`, launch `agy` once for interactive login, and list slugs
with `agy models`. Saved login is under `~/.gemini/antigravity-cli/`; this route
needs no token name in `execution.env.pass`. Writes cover `~/.gemini`; macOS
reads the keychain. Version-dependent alternative key auth requires the CLI's
Gemini model-provider setting as well as allowing `GEMINI_API_KEY`; see its
[authentication instructions](https://antigravity.google/docs/install?tab=cli).
Native MCP uses `orbit mcp init --antigravity` (`.agents/mcp_config.json`, or
`~/.gemini/config/mcp_config.json` at home scope). `authentication required`
means finish login on that host. This lane uses `agy`, separate from `gemini`;
when migrating, change both the provider and model slug.

### Muse

Install `curl -fsSL https://dev.meta.ai/install.sh | sh` per the
[Muse Code installer](https://dev.meta.ai/docs/muse-code) and verify
`muse --version` on `PATH` (`~/.local/bin/muse` launcher). Run `muse login`
outside Orbit, or allow `META_API_KEY`, which takes precedence. In interactive
Muse, [`/models`](https://dev.meta.ai/docs/muse-code/configuration) lists choices;
`muse model-profile show <model>` resolves a
catalog ID. These details depend on the CLI version (Orbit's headless contract
was checked with Muse 1.4.10). Preserve customized `XDG_CONFIG_HOME` and
`XDG_DATA_HOME` in `pass`: writes cover their `muse` subdirectories (defaults
`~/.config/muse`, `~/.local/share/muse`), plus macOS
`~/Library/Application Support/Muse`; macOS reads `~/Library/Keychains`.
`missing meta credentials` calls for checking login/key availability and keychain
access. Orbit fixes `MUSE_NO_AUTO_UPDATE=1` and `MUSE_LOGIN=0` for runs; upgrade
and login outside them. Muse has native MCP, but Orbit's per-run injection is
unverified and init has no Muse target: keep shell access for `orbit tool run`.

