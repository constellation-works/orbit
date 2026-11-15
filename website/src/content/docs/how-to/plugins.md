---
title: Install and grant plugins
description: "Install an Orbit plugin on this machine, grant the permissions it needs, enable it, and upgrade or remove it."
---

A plugin adds tools to Orbit on this machine. It can also ship activities, jobs,
routines, skills and a settings section. Enabling a plugin starts nothing: the
routines and auto-tasks it ships stay off until you switch them on.

To add Orbit itself to Claude Code or Codex, see
[Install as an agent plugin](/getting-started/install/#install-as-an-agent-plugin).

## Install and enable

The examples use a local source, `./my-plugin`. Its manifest names the namespace
`notes` and requests the `fs` and `network` permissions.

```bash
orbit plugin validate ./my-plugin               # check the manifest, install nothing
orbit plugin add ./my-plugin                    # install for this machine, disabled
orbit plugin show notes                         # tools, requested and granted permissions
orbit plugin enable notes --grant fs,network    # grant and enable
```

A manifest's permissions are a request. Nothing it asks for is active until you
grant it, so read `show` before you enable.

Enabling records the grants, seeds what the plugin ships and names any declared
secret that is still unset. The tools register on the next Orbit command. A
running `orbit mcp serve` session picks them up after a restart.

To install and enable in one step:

```bash
orbit plugin add ./my-plugin --enable --grant fs,network
```

`orbit plugin validate ./my-plugin --render` also prints the sandbox profile and
child environment the backend would receive, without running it.

## Plugin sources

Plugins install once per machine, under `~/.orbit/plugins/<namespace>/<version>/`.
A plugin tree is never copied into a repository. A source is one of:

- a directory holding `.orbit-plugin/plugin.yaml`
- a `git+<url>#<ref>` reference
- a local `.tar.gz`, `.tgz`, `.tar` or `.zip` archive
- an `https://` archive URL, with `--digest sha256:<hex>`. Orbit does not install
  a downloaded archive without the hash.

A `git+<url>#<full commit id>` source whose manifest declares a build step is
refused unless you pass `--allow-build`. The refusal prints the build plan to
review. The flag covers that one command.

## Choose grants

| Grant | What it lets the plugin do |
|---|---|
| `fs` | Read and write the roots its manifest requests. Narrow it with `fs=<root>[,<root>]`. |
| `network` | Reach the network at the level its manifest requests (`loopback` or `any`). Without it, TCP connections are refused. |
| `env_pass` | Receive the environment variables its manifest names. |
| `orbit_tools` | Call the Orbit tools its manifest lists, and no others. |
| `unsandboxed` | Run with no confinement. Grant it only for a plugin you have reviewed. |
| `agent_tools` | Let agent sessions call the mutating tools the manifest marks `agent_callable: true`. It grants no operator authority. |

- `--grant` is the complete set and replaces the recorded one. Omitting it keeps
  the recorded set. `--grant none` revokes every grant.
- `all` grants everything and `requested` grants what the manifest asks for.
  Prefer naming each grant, so each one is a decision.

## Diagnose inactive tools

```bash
orbit plugin doctor
orbit plugin show notes
```

`doctor` prints one row per plugin, with its state and the step that makes it
active:

| State | Meaning | Fix |
|---|---|---|
| `active` | Installed, enabled and serving its tools. | |
| `disabled` | Not enabled on this machine, or switched off in this workspace. | Run the `enable` command `doctor` names. |
| `missing` | Pinned by this checkout but not installed here. | `orbit plugin sync` |
| `inactive` | Enabled but refused on load: the manifest no longer loads, its Orbit or host API requirements do not hold, its namespace collides with another plugin, or its settings fail its schema. | `orbit plugin show notes` names the reason. |

A tool whose plugin requests a grant you have not given registers inactive, and
calling it is refused. The refusal names the missing grant. Enable again with
the complete set:

```bash
orbit plugin enable notes --grant fs,network
```

## Switch it off in one workspace

A workspace can switch a host-enabled plugin off for itself, and back on:

```bash
orbit plugin disable notes --scope workspace   # off in this workspace only
orbit plugin enable notes --scope workspace    # back on here
```

Run these from the checkout, or pass `--workspace <selector>`. The plugin's
tools, routines, auto-tasks and dashboard panels disappear from this workspace
only. The toggle is stored in the workspace's `.orbit/config.toml`:

```toml
[plugin_enablement]
notes = false
```

- The host state is the ceiling. `enable --scope workspace` is refused while the
  host has the plugin disabled.
- `--scope workspace` never records grants, so `--grant` is refused with it.
- `[plugin_enablement]` is refused in the global `config.toml`.

To switch a plugin off in every workspace on this machine and unlink its skills:

```bash
orbit plugin disable notes
```

## Set secrets

A plugin declares the credentials it needs in `spec.secrets`. Set them here, not
in plugin settings.

```bash
orbit plugin secret set notes api_token    # prompts with no echo, or reads stdin
orbit plugin secret list notes             # names and whether each is set, never values
orbit plugin secret rm notes api_token
```

- `set` takes no value argument, so the value never reaches shell history.
- Only names the installed manifest declares can be set.
- Orbit keeps the values on this machine and never prints them. A backend
  receives only its own plugin's secrets.
- `enable` and `doctor` report declared secrets that are still unset.

Plugin settings are separate: `orbit config set plugins.notes.<key> <value>`
accepts only keys the plugin declares. See
[Settable keys](/reference/config/#settable-keys).

## Pin plugins in a checkout

A checkout can list the plugins it uses in `.orbit/plugins.yaml`. The file is per
checkout and gitignored with the rest of `.orbit/`:

```yaml
schemaVersion: 1
plugins:
  - name: notes
    source: git+https://example.com/you/my-plugin#v0.1.0
    enabled: true
```

```bash
orbit plugin sync --dry-run    # report what this machine is missing
orbit plugin sync              # install it
```

A pin never grants a permission. `enabled: false` switches the plugin off in
this workspace only.

## Upgrade

```bash
orbit plugin upgrade notes ./my-plugin-v0.2
```

- Name the replacement source on the command. Orbit does not reuse the recorded
  one, and an `https://` archive needs its own `--digest`.
- The output lists each changed permission request as `old -> new` and marks a
  wider one `(widened)`.
- A widened request disables the plugin and clears its grants. Orbit prints the
  `enable --grant` command to consent again. To consent in the same step, pass
  the complete set you reviewed:

  ```bash
  orbit plugin upgrade notes ./my-plugin-v0.2 --grant fs,network
  ```

- An unchanged or narrower request keeps the grants and enable state.
- Secrets the new manifest still declares are kept.

## Remove

```bash
orbit plugin remove notes --confirm
```

| Flag | Effect |
|---|---|
| `--confirm` | Required. Removes the install and deletes the plugin's secrets. |
| `--purge-state` | Also deletes the plugin's Orbit-owned state directory. |
| `--record-only` | Removes only this machine's record. Installed files and secrets stay. Use it for a record Orbit cannot clean up itself. |

Routines and auto-tasks the plugin seeded into your workspaces stay. Orbit skips
them while the plugin is absent.

## Related

- [Plugin manifest](/reference/plugin-manifest/): write a plugin.
- [CLI commands](/reference/cli/): every `orbit plugin` verb.
