# Plugins: legacy external tools

## Contents

- [Migrating an existing external tool](#migrating-an-existing-external-tool)
- [Managing legacy external tools](#managing-legacy-external-tools)

## Migrating an existing external tool

The older form — one executable plus one `*.orbit-tool.yaml` sidecar per tool,
registered with `orbit tool add` — keeps working unchanged. To move a set of
sidecars onto one manifest:

```bash
orbit plugin migrate ./bin/my-tool --version 0.1.0 --out-dir ./my-plugin
orbit plugin validate ./my-plugin
```

Review the generated manifest before installing it: migration cannot know
which tools are read-only, so every migrated tool is marked `mutating`.
Migration also cannot prove first-party provenance. An old
`orbit.graph.recommend` sidecar therefore produces a manifest with namespace
`graph`, no `publisher` or `origin`, and a `graph.recommend` tool. A local
directory's own Git remote is never proof of origin.

The author can use the
reserved `orbit.graph.*` namespace only by adding
`publisher: constellation-works` and `origin: orbit` and then distributing the
plugin through a verified `git+https://github.com/constellation-works/...`
source; a manifest digest bundled into an Orbit release is the other trusted
path.

## Managing legacy external tools

The older form — one executable plus one `*.orbit-tool.yaml` sidecar per tool,
registered with `orbit tool add` — keeps working unchanged, with its own
retained controls. These commands manage per-tool stored rows. They never grant
plugin permissions: grants come only from `orbit plugin enable --grant …` (see
[Permissions and grants](plugins-permissions.md#permissions-and-grants)), and `orbit tool enable` /
`orbit tool disable` record no grant.

A plugin tool that is `inactive` for a
missing grant still needs that plugin grant; a tool toggle cannot fix it (see
[When a plugin is not serving its tools](plugins-certify.md#when-a-plugin-is-not-serving-its-tools)).
A tool toggle is likewise not a plugin switch: switching a plugin off in one
workspace stays `orbit plugin disable <ns> --scope workspace` (see [Switching a
plugin off in one workspace](plugins-install.md#switching-a-plugin-off-in-one-workspace)).

```bash
orbit tool add ./bin/my-tool --description "What it does"
orbit tool list
orbit tool list --all
orbit tool show my-tool
orbit tool run my-tool --input '{}'
orbit tool run my-tool --dry-run --input '{}'
orbit tool disable my-tool
orbit tool enable my-tool
orbit tool remove my-tool
orbit tool doctor
```

- `add <PATH> [--name] [--description] [--manifest]` registers the executable
  at `PATH`, starting enabled. The path must exist. The name is `--name`,
  else the sidecar manifest name, else the filename stem. The description is
  `--description` when non-empty, else the manifest description, else empty.
  `list` and `show` append the external-tool runtime contract to the stored
  description.

  `--manifest` names an explicit sidecar; otherwise
  the sibling `<stem>.orbit-tool.yaml` (also `.yml` or `.json`) beside the
  executable is used when present. A name that would overwrite a built-in tool
  is refused.
- `list` shows the registered tools; `list --all` also includes the inactive
  tools hidden from the default agent surface. The `STATUS` column reads
  `inactive` when the tool is not active, `disabled` when it is not enabled,
  else `active`. A stored external tool counts as active, so a disabled
  external reads `disabled`.
- `show <name>` prints the name, description, built-in flag, status and
  parameters. A disabled external stays inspectable here.
- `run <name>` executes the tool. `--dry-run` checks admission and required
  parameters without executing. A disabled external is absent from the
  executable registry, so both report it as not found; a disabled built-in or
  plugin-registered tool stays registered and refuses naming
  `orbit tool enable <name>`.
- `disable <name>` / `enable <name>` is a per-tool override that takes effect
  on the next command. Disabling blocks `run` for any tool with a stored row,
  including a plugin-registered tool, and drops that tool from MCP
  `tools/list`; a stored external is loaded only while enabled. The disabled
  entry stays listed: `list` reads `disabled`, `show` still inspects it, and
  `doctor` warns it is disabled. Built-in tools can be disabled.

  Enabling or
  disabling an inactive tool that is already in the requested state is refused
  as a no-op.
- `remove <name>` deletes only the stored row: it never deletes the executable
  or sidecar files and never uninstalls a plugin. It refuses built-in tools
  (disable those instead) and reports an unknown name as not found. An absent
  row reads as enabled, so removing a disabled plugin-registered row
  re-enables that tool.
- `doctor` is read-only and never runs a tool. One row per tool: a warning
  when the tool is disabled, a warning when its description is missing, an
  error when its stored executable is missing, else ok. An external added
  without a description still shows the appended runtime contract, so it
  reports ok rather than the missing-description warning.

Verify a legacy external tool without touching live state:

1. `orbit tool add` a throwaway executable with a description and confirm the
   `Added tool …` reply.
2. `orbit tool list` and `orbit tool show <name>` report it `active` with the
   expected description and parameters; `orbit tool list --all` includes the
   inactive entries the default list hides.
3. `orbit tool run <name> --dry-run --input '{}'` reports the admission check
   without executing, then a real `orbit tool run <name>` executes.
4. `orbit tool doctor` reports the tool ok.
5. `orbit tool disable <name>`, then confirm `list` reads `disabled`, `run`
   reports it as not found, and `doctor` warns it is disabled.
6. `orbit tool enable <name>`, then confirm `list` reads `active` again.
7. `orbit tool remove <name>`, then confirm it is gone from `list`.
