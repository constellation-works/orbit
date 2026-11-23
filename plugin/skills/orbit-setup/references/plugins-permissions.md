# Plugins: permissions, grants and secrets

## Contents

- [Permissions and grants](#permissions-and-grants)
- [Secrets](#secrets)

## Permissions and grants

The manifest's `spec.permissions` block is a **request**, never a grant.
`--grant` on an enabling add, enable, upgrade or sync is the only source of authority, and
`orbit plugin show` prints requested and granted side by side. Record only the
grants the user authorizes.

| Grant | What the manifest asks for | What granting it does |
|---|---|---|
| `fs` | `permissions.fs.read` / `.write` / `.write_files` | Opens exactly those paths to the sandboxed backend. `write_files` opens only the named existing files, never their parent directory (see below). |
| `network` | `permissions.network: loopback\|any` | Lets the backend reach the network; without it, TCP is refused. |
| `env_pass` | `permissions.env_pass` | Copies those variables from Orbit's environment into the child. `ORBIT_*` names are reserved for Orbit's own envelope: `validate_structure` refuses a manifest that names one, and the privilege-bearing ones (`ORBIT_OPERATOR`, `ORBIT_WORKSPACE_CLAIM_TOKEN`) can never reach the child even so. |
| `orbit_tools` | `permissions.orbit_tools` | Lets the backend call those Orbit tools back through `orbit tool run`, and nothing else. |
| `unsandboxed` | `backend.sandbox: none` | Runs the backend with no confinement at all. Agents in a sandboxed run cannot call it; the broker refuses the call. |

`permissions.fs.write_files` is the exact-file write request. Use it for an existing file the
backend must modify, such as a runtime file named by `{{config.<key>}}`, without also opening
its directory:

```yaml
permissions:
  fs:
    write_files: ["{{config.runtime_file}}"]
```

Each entry must render to an absolute physical path of an existing regular file that the
current user owns, with one hard link and no write bit for others (the one exception is a
`{{workspace}}` link to such a file, below). A group write bit is
accepted, so a checkout made under umask 0002 works; so is a group-writable parent
directory the user owns. Spell it physically: on macOS, `/private/tmp/...` rather than the
`/tmp` alias.

Missing files, directories, and paths that pass through a symbolic link or an
unsafe directory are refused.

To rewrite one file at the workspace root, such as `AGENTS.md`, grant it by template:

```yaml
permissions:
  fs:
    write_files: ["{{workspace}}/AGENTS.md"]
```

`{{workspace}}` renders to the workspace the call comes from; for an agent in a managed
run that is the task's worktree. The file itself may be a symbolic link, as long as the
link is inside the workspace and its target is a regular file inside the same workspace,
outside `.git` and `.orbit` (for example `AGENTS.md` -> `CLAUDE.md`). The sandbox then
grants the target, so the backend writes through the link and the link stays.

Write the
file in place (truncate and write); replacing it by rename needs a directory grant. The
grant never reaches `.git/`, `.orbit/`, any other file at the root, or a file outside the
workspace, and a link whose target is outside the workspace, missing, or inside `.git` or
`.orbit` is refused. Without a workspace, a symbolic link is always refused.
`orbit plugin test` creates each such file, empty, in its scratch workspace before the
cases run, so the grant can be tested there.

Outside `orbit plugin test`, Orbit never creates such a
file or widens its grant: creating or replacing a file by rename needs a `fs.write` directory grant. A scoped `fs=<root>` grant
covers a `write_files` entry only when it names that file or one of its ancestor
directories. Adding an entry, or changing where a config value resolves, needs consent
again. Linux binds the existing inode, and macOS uses a file-rooted `subpath` rule.
Neither grants sibling files.

Admission refuses a path with `*` or `?` in any component,
whether the file name or an ancestor directory: macOS reads those characters as wildcards.

A plugin that requests a grant the host has not given registers its tools
**inactive**: a call is refused with a diagnostic naming the grant and the
`orbit plugin enable --grant …` that would fix it. Granting is per host and
never synced.

Backends run confined: on Linux under a Landlock ruleset built from the
granted paths, on macOS under `sandbox-exec`. A write outside the granted
profile fails; so does a TCP connection without the `network` grant. A
`sandbox: none` plugin needs `unsandboxed` and is reported by
`orbit plugin doctor` for as long as it stays enabled.

Callbacks are the only way back into Orbit. The host hands the backend its
per-call session record on file descriptor 3, inherited by descendants;
`ORBIT_PLUGIN` and `ORBIT_ALLOWED_TOOLS` are information for the child, not the
gate. `orbit tool run` and MCP `tools/call` verify that descriptor against the
host-owned session record and refuse anything outside its effective tool ceiling.
Closing the descriptor is a refusal; changing environment variables or using
`setsid` cannot expand access.

The retired environment token and process
ancestry never authorize callbacks; backends must preserve the descriptor.

A `read_only` plugin tool is callable by an identified agent, operator or runner.
A `mutating` tool requires operator or runner capability unless its manifest
sets `agent_callable: true` and the operator has granted `agent_tools`:

```yaml
- name: capture
  execution_kind: mutating
  agent_callable: true
  mcp_scope: workspace
```

Use the declaration only for append-only or proposal-only tools whose writes
are appropriate for an agent session. It defaults to false; true on a read-only
tool is invalid. Review the backend before granting it: Orbit checks consent,
not whether the implementation is append-only.

`orbit plugin enable <ns> --grant agent_tools` consents to the declared verbs
at that manifest digest. Include every other needed grant in the same list,
or use `--grant requested` after reviewing the requests. Missing consent keeps
the plugin inactive. Enable output names these tools and the grant's status;
`plugin list` names them and `plugin show` displays their declarations and
requested versus granted permissions.

Adding another agent-callable verb on
upgrade widens permissions and requires fresh consent, even when `agent_tools`
was already granted. Older Orbit hosts reject the new field or grant.

A consented tool is callable over `orbit mcp serve` without `--operator`,
through `orbit tool run`, and through its derived CLI command, with the caller
recorded in the audit trail. The declaration grants no operator authority,
filesystem access or callback permission. Activity allowlists and denials still
apply; `required_tools` and allowlist membership alone grant no capability.
Unmarked mutations keep their existing operator/runner requirement.

`ORBIT_OPERATOR=1` and `--operator` keep their existing behavior.

Treat an installed plugin as code the user has chosen to run on their machine,
and say so when proposing one: the sandbox bounds what a backend can reach, it
does not vouch for what the backend does inside those bounds.

## Secrets

A plugin that talks to an authenticated service declares the credentials it
needs by name in its manifest, and the user sets the values on this machine.
Never put a credential in `[plugins.<ns>]` config or ask for it in a prompt.

```yaml
spec:
  secrets:
    - name: refresh_token          # lowercase letter, then a-z 0-9 _ - (max 64)
      description: OAuth refresh token.
      rotatable: true              # the backend may replace it
```

```bash
orbit plugin secret set <ns> refresh_token < token.txt   # or run it in a terminal to be prompted
orbit plugin secret list <ns>                            # names, set/unset, updated-at — never values
orbit plugin secret rm <ns> refresh_token
```

- The value comes from stdin or a prompt that does not echo. A value typed on
  the command line is refused: argv is readable by other processes and lands
  in shell history. Only names the installed manifest declares can be set.
- Values live in `~/.orbit/state/plugin-secrets/<ns>.json` (mode `0600`). No
  plugin backend can read that directory, its own file included; Orbit reads a
  value and hands it over. A secret value never appears in CLI output,
  `plugin show`, the dashboard, logs or audit rows.
- Each call carries the plugin's own declared secrets that are set, in the
  request itself: an `exec` backend reads `context.secrets` on stdin, an `mcp`
  backend `params._meta.orbit.secrets` on `tools/call`. Each entry is
  `{"value": …, "version": …}`, where `version` is opaque and changes on every
  write. An unset secret is simply absent, so a backend must handle that
  (`secrets` itself is present exactly when the manifest declares secrets).

  Values are read per call, so a `secret set` reaches a running `mcp` server's
  next call. Nothing goes into the backend's environment or argv, no grant is
  needed, and a plugin never receives another plugin's secrets. The call's
  audit row records which names were delivered, never values.
- `orbit plugin enable` names every declared secret that is still unset, and
  `orbit plugin doctor` keeps reporting them. `upgrade` keeps the secrets the
  new manifest still declares; `remove` deletes the plugin's secrets unless
  `--record-only` is passed.
- These are operator commands: a plugin backend calling them is refused
  `policy_denied`.
- A backend rotates a secret it declares `rotatable: true` by answering with
  `secret_updates` — beside `ok`/`output` on `exec` stdout, as
  `result._meta.orbit.secret_updates` on an `mcp` result:
  `{"<name>": {"value": "<new>", "expected_version": "<version it was delivered>"}}`
  (`null` writes only while the secret is unset). Orbit applies it with
  compare-and-swap, so of two calls rotating from one version exactly one
  wins.

  An undeclared or non-rotatable name, a malformed entry or a stale
  version is refused with a warning that names the secret but never a value,
  and the call still returns its output (or its error). Updates are applied
  even when the call reports `ok: false`, so a token refreshed before a failed
  request is kept. The next call's `secrets` shows what is stored, and the
  audit row records each name as `applied` or `refused`. `orbit plugin test`
  refuses every update.

  OAuth refresh with X, which invalidates the old refresh token on every
  refresh:

  ```json
  {"ok": true, "output": {"posted": "…"},
   "secret_updates": {"refresh_token": {"value": "<new refresh token>",
                                        "expected_version": "<context.secrets.refresh_token.version>"}}}
  ```
