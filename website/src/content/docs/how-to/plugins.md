---
title: Install and grant plugins
description: "Add an Orbit plugin to this machine, grant only the permissions it requests, enable it, switch it off in one workspace, diagnose inactive tools, and upgrade or remove it."
---

A plugin adds tools to Orbit on this machine. It can also contribute activities,
jobs, routines, skills and a settings section. Add one only when you need the
capability it provides. Enabling a plugin starts nothing: the routines and
auto-tasks it ships stay off until you review them and switch them on.

This guide covers Orbit plugins, installed with `orbit plugin`. To install Orbit
itself as a Claude Code or Codex plugin, see
[Install as an agent plugin](../../getting-started/install/#install-as-an-agent-plugin).
That step adds Orbit to your agent. A plugin adds a capability to Orbit.

## Machine installs and checkout pins

Two things are involved, and they are kept apart:

- **Machine install.** `orbit plugin add` installs a plugin once per machine, under
  `~/.orbit/plugins/<namespace>/<version>/`. Enable state and grants are recorded on
  this machine. A plugin tree is never copied into a repository.
- **Checkout pin.** A checkout can list the plugins it uses in `.orbit/plugins.yaml`.
  That file is per checkout and gitignored with the rest of `.orbit/`:

  ```yaml
  # .orbit/plugins.yaml — per checkout, gitignored
  schemaVersion: 1
  plugins:
    - name: notes
      source: git+https://example.com/you/my-plugin#v0.1.0
      enabled: true
  ```

  `orbit plugin sync --dry-run` reports what this machine is missing, and
  `orbit plugin sync` installs it. A pin never grants a permission. Pin content is never
  consent. `enabled: false` switches the plugin off in this workspace only.

A plugin source is one of these:

- a directory holding `.orbit-plugin/plugin.yaml`
- a `git+<url>#<ref>` reference
- a local `.tar.gz`, `.tgz`, `.tar` or `.zip` archive
- an `https://` archive URL, which needs `--digest sha256:<hex>`. Orbit never installs
  a downloaded archive whose hash you have not given.

The examples below use an illustrative local source, `./my-plugin`. Its layout looks
like this:

```text
my-plugin/
└── .orbit-plugin/
    ├── plugin.yaml      # the manifest: namespace, tools and permission requests
    ├── bin/             # the backend the manifest runs
    └── skills/          # optional agent skills
```

The manifest names the plugin's namespace and requests permissions under
`spec.permissions`. In this example the namespace is `notes`:

```yaml
# my-plugin/.orbit-plugin/plugin.yaml (excerpt, illustrative)
metadata:
  name: notes
spec:
  permissions:
    fs:
      read: ["{{workspace}}"]
    network: loopback
```

A manifest's permissions are a request, never a grant. Nothing it asks for is active
until you grant it.

## Check the source

```bash
orbit plugin validate ./my-plugin
orbit plugin validate ./my-plugin --render
```

`validate` checks the manifest without installing anything. `--render` also prints the
sandbox profile and child environment the backend would receive. It does not run the
backend.

## Install

```bash
orbit plugin add ./my-plugin
```

`add` installs the plugin for this machine, disabled. Its output names
`orbit plugin enable` as the next step.

A `git+<url>#<full commit id>` source whose manifest declares a build step is refused
unless you pass `--allow-build` on that command. The refusal prints the build plan for
review. Consent comes only from that flag, never from a pin or from config.

## Inspect requested permissions

```bash
orbit plugin show notes
```

`show` prints the plugin's tools and its requested and granted permissions side by side.
Review the requests before you grant any of them.

## Grant only what you need, then enable

| Grant | What it lets the plugin do |
|---|---|
| `fs` | Read and write the roots its manifest requests. Narrow the grant with `fs=<root>[,<root>]`. |
| `network` | Reach the network, at the level its manifest requests (`loopback` or `any`). Without it, TCP connections are refused. |
| `env_pass` | Receive the environment variables its manifest names. |
| `orbit_tools` | Call the Orbit tools its manifest lists, and no others. |
| `unsandboxed` | Run with no confinement. Grant it only for a plugin you have reviewed. |

Grant only the entries the plugin needs. The `notes` manifest requests two grants, so
grant those two and nothing else:

```bash
orbit plugin enable notes --grant fs,network
```

- `--grant` is the complete set. It replaces the recorded set, and omitting it keeps
  the recorded set. `--grant none` revokes every grant.
- The reserved words `all` and `requested` grant every grant or everything the manifest
  asks for. Name each grant instead, so each one is a decision.
- A tool whose plugin requests a grant you have not given registers inactive. See
  [Diagnose inactive tools](#diagnose-inactive-tools).

Enable records the plugin's grants, seeds what it ships, and reports any declared
secret that is still unset. Its tools register on the next Orbit command. A
long-lived `orbit mcp serve` session picks them up after a restart.

To grant on install instead of in a second step, add `--enable` and `--grant` to `add`:

```bash
orbit plugin add ./my-plugin --enable --grant fs,network
```

## Diagnose inactive tools

```bash
orbit plugin doctor
orbit plugin show notes
```

`doctor` prints one row per plugin, with its state and the step that would make it active:

- `active`: installed, enabled and serving its tools.
- `disabled`: installed but not enabled on this machine, or switched off in this
  workspace. `doctor` names the `enable` command for each case.
- `missing`: pinned by this checkout but not installed here. Run `orbit plugin sync`.
- `inactive`: enabled but refused when it loads. The manifest no longer loads, its
  Orbit or host API requirements do not hold here, its namespace collides with another,
  or its settings fail its own schema. `orbit plugin show notes` names the reason.

A tool whose grant is missing is refused when called. The refusal names the grant and
the `orbit plugin enable --grant …` command that fixes it. `show` lists requested and
granted permissions side by side, so the missing grant is visible there. Fix it by
enabling again with the complete set you have reviewed, for example:

```bash
orbit plugin enable notes --grant fs,network
```

## Switch it off in one workspace

Host state applies to every workspace on this machine. A workspace can switch a
host-enabled plugin off for itself, and can switch it back on:

```bash
orbit plugin disable notes --scope workspace   # off in this workspace only
orbit plugin enable notes --scope workspace    # back on here
```

Run these from the checkout, or pass `--workspace <selector>`. The host state is the
ceiling, and a workspace can only narrow it.

- The toggle is a `[plugin_enablement]` table in the workspace's `.orbit/config.toml`:

  ```toml
  # .orbit/config.toml — this workspace only
  [plugin_enablement]
  notes = false
  ```

- Other workspaces keep the plugin on. Its tools, catalog entries, routines, auto-tasks
  and dashboard panels disappear from this workspace only.
- Writing the table never resets your sandbox, approval or environment settings. A
  workspace file that holds only this table keeps inheriting those settings from your
  global config.
- `--scope workspace` never records grants, so `--grant` is refused with it.
- `enable --scope workspace` is refused while the host has the plugin disabled. Enable
  it on the host first.
- `[plugin_enablement]` is refused in the global `config.toml`.

To switch a plugin off for every workspace on this machine, disable it on the host. This
also unlinks its skills:

```bash
orbit plugin disable notes
```

## Set secrets

A plugin declares the credentials it needs in `spec.secrets`. Set their values on this
machine. Do not put a credential in plugin settings.

```bash
orbit plugin secret set notes api_token    # prompts with no echo
orbit plugin secret list notes             # names and whether each is set, never values
orbit plugin secret rm notes api_token
```

- `set` takes the plugin and the secret name only. It reads the value from a no-echo
  prompt or from stdin, and there is no argument for the value. A value on the command
  line would appear in shell history.
- Only names that the installed manifest declares can be set.
- Orbit keeps the values on this machine and never prints them. Plugin backends receive
  only their own secrets, in each call.
- `enable` names declared secrets that are still unset, and `doctor` keeps reporting
  them.

Plugin settings are set separately, with `orbit config set plugins.notes.<key> <value>`.
Only keys the plugin declares are accepted. See
[Settable keys](../../reference/config/#settable-keys).

## Upgrade

```bash
orbit plugin upgrade notes ./my-plugin-v0.2
```

- The replacement source is required and must be named on the command. Orbit does not
  reuse the recorded source. An `https://` archive needs its own `--digest`.
- The output lists each requested permission that changed as `old -> new`, and marks a
  widened one `(widened)`.
- A widened request disables the plugin and clears its grants. The widened requests are
  filesystem roots, network mode, environment names, Orbit tool callbacks and the
  sandbox. Orbit then prints the `orbit plugin enable --grant …` command to re-consent.
  To re-consent in the same step, pass the complete set you reviewed:

  ```bash
  orbit plugin upgrade notes ./my-plugin-v0.2 --grant fs,network
  ```

- An unchanged or narrower request keeps the existing grants and enable state.
- Secrets that the new manifest still declares are kept.

## Remove

```bash
orbit plugin remove notes --yes
```

- `--yes` confirms the permanent removal of the install. Removal also deletes the
  plugin's secrets.
- `--purge-state` also deletes the plugin's Orbit-owned state directory.
- `--record-only` removes only this machine's record and leaves the installed files in
  place. It also keeps the secrets. Use it for a record that Orbit cannot clean up
  itself.
- Routines and auto-tasks that the plugin seeded into your workspaces stay where they
  are. Orbit skips them while the plugin is absent.

## Related

- [Plugin manifest](../../reference/plugin-manifest/) covers authoring, backend
  contracts, conformance cases, dashboard panels and legacy-tool migration.
- [CLI commands](../../reference/cli/) lists every `orbit plugin` verb.
- [Configuration](../../reference/config/) covers plugin settings and the workspace
  `config.toml`.
- [Install Orbit](../../getting-started/install/) covers installing Orbit itself and the
  agent plugin.
