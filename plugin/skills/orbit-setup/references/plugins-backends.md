# Plugins: backends

## Contents

- [Backends](#backends)
- [Python backends with dependencies](#python-backends-with-dependencies)
- [Backends built from source](#backends-built-from-source)

## Backends

| `backend.type` | How Orbit runs it |
|---|---|
| `exec` | One process per call. Orbit writes `{"schema_version":1,"tool":…,"input":…,"context":{…}}` on stdin and reads `{"ok":true,"output":…}` or `{"ok":false,"error":{…}}` on stdout. A non-zero exit, non-JSON stdout, or output failing the tool's `output_schema` is a tool error — never a partial result. |
| `mcp` | The plugin ships a stdio MCP server. Orbit spawns one per caller context per runtime process — a caller in another workspace, or with a different allowed-tools intersection, gets its own child rather than one confined to the first caller's workspace — checks its `tools/list` against the manifest (a disagreement refuses startup, naming the tool), and proxies each `<ns>.<verb>` call as `tools/call`. A crashed or unresponsive server is a tool error within `backend.timeout_ms`, and the next call respawns it. |

## Python backends with dependencies

The scaffold's backend uses only the standard library. A Python backend that
needs third-party packages ships as a uv project, and uv builds the environment
under the plugin's state directory on first call. Do not commit a virtualenv:
it symlinks its interpreter, and `plugin add` refuses a tree that contains a
symbolic link. The uv project lives inside the plugin root, because nothing
outside `.orbit-plugin/` is installed.

```text
my-plugin/
  .orbit-plugin/
    plugin.yaml
    pyproject.toml     # [project] dependencies; [tool.uv] package = false
    uv.lock            # committed: pins every dependency
    .python-version    # optional: an interpreter uv may download into plugin state
    backend/main.py
    bin/my-plugin      # the shim below, executable
```

```yaml
spec:
  requires:
    programs: [uv]
  backend:
    type: exec
    command: bin/my-plugin
    timeout_ms: 60000        # the first call installs the environment
  permissions:
    fs:
      write: ["{{plugin_state}}"]
    network: any             # the first sync downloads the locked wheels
```

```sh
#!/bin/sh
set -eu
state="${ORBIT_PLUGIN_STATE:?ORBIT_PLUGIN_STATE is not set}"
export UV_PROJECT_ENVIRONMENT="$state/venv"
export UV_CACHE_DIR="$state/uv-cache"
export UV_PYTHON_INSTALL_DIR="$state/uv-python"
export UV_PYTHON_BIN_DIR="$state/uv-python/bin"
unset PYTHONPYCACHEPREFIX
export PYTHONDONTWRITEBYTECODE=1     # the plugin root is read-only
export TMPDIR="$state/tmp"
mkdir -p "$TMPDIR"
exec uv run --frozen --exact --no-dev --quiet --project "$ORBIT_PLUGIN_ROOT" \
  python "$ORBIT_PLUGIN_ROOT/backend/main.py"
```

Enable it with `orbit plugin enable my-plugin --grant fs,network`, and certify
it with `orbit plugin test ./my-plugin --grant network`. `uv` is resolved from
`PATH` when the plugin is enabled and granted at that path. `orbit plugin show`
lists it under Programs, and `orbit plugin doctor` reports it when it has gone.
No other grant or sandbox exception is involved. uv reads the plugin root, its
own binary, and the system runtime directories the sandbox already allows.

Every write lands under `{{plugin_state}}`. On Linux, Landlock refuses a few
reads uv can do without, such as its user configuration under `$HOME` and some
`/proc/self` files; uv continues. Do not set `UV_NO_CONFIG=1`, because it also
makes uv ignore `.python-version`. Under Landlock the interpreter uv picks must
also be readable. A system Python qualifies, and so does one uv downloads into
plugin state. A Python installed under `$HOME`, for example by pyenv, does not.

On such a host, add `export UV_PYTHON_PREFERENCE=only-managed` to the shim so uv
always uses an interpreter it downloaded into plugin state.

**Network.** The first call downloads the locked wheels, and the interpreter
when `.python-version` names one the host lacks. The first call after a
lockfile change downloads whatever that change added. Both need `network: any`
and the `network` grant; without them a cold call fails with a connection error
from uv. Other calls use the cache and open no connection. A plugin that ships
its wheels in its own tree, locked through `[tool.uv.sources]` path entries,
needs no network at all.

**Cost.** On one Linux host with Orbit 0.24.0, `orbit <ns> <verb>` took these
times:

| Call | Time |
|---|---|
| Cold, 11 wheels (19 MB) into an empty state directory | about 0.45 s |
| Cold, also downloading a CPython | about 2 s |
| Warm | 0.14–0.15 s |
| The same environment's `python` without uv | 0.11–0.12 s |

`--offline` and `--no-sync` add nothing measurable, and `--offline` breaks the
cold call. Concurrent cold calls are safe because uv locks its cache and the
environment.

**Upgrades.** The environment is not part of the install. Every call runs
`uv run --frozen --exact`, which compares the state-directory environment with
the installed `uv.lock`. After `orbit plugin upgrade` installs a changed lock,
the next call adds, upgrades and removes packages to match it.

Disable Python
bytecode caching as in the shim above: a shared `PYTHONPYCACHEPREFIX` can reuse
old bytecode after an upgrade when the old and new source have the same size
and second-resolution timestamp, even though uv installed the new wheel. The
manifest digest covers `plugin.yaml` only. An upgrade that changes just the
lockfile therefore reports the requested permissions as unchanged and keeps
the plugin's grants.

Review the lock diff
before upgrading, because it is new code even though it is not a new
permission. `orbit plugin remove --purge-state` deletes the environment and
cache with the rest of the plugin's state.

## Backends built from source

A prebuilt archive pinned by digest is the recommended way to ship a compiled
backend. A plugin that cannot publish one per platform can declare
`spec.build`, and Orbit builds it on the installing host:

```yaml
spec:
  backend:
    type: exec
    command: bin/orbit-graph            # produced by the build
  build:
    programs: [cargo]                   # resolved from PATH and shown at consent
    fetch: [cargo, fetch, --locked]     # optional; the only phase with network; Linux only
    command: [cargo, build, --release, --offline, --locked, --target-dir, "{{build_dir}}/target"]
    outputs:
      - from: target/release/orbit-graph   # relative to {{build_dir}}
        to: bin/orbit-graph                # relative to the plugin root
    timeout_ms: 1200000                    # per phase; at most 3600000
```

`fetch` and `command` are argv arrays and never pass through a shell.
`{{build_dir}}` is the only template, and `fetch[0]` and `command[0]` must be
listed in `programs`.

**Which sources build.** Only `git+<url>#<full commit id>` (40 or 64 hex
characters). Orbit fetches that one commit, verifies it, and drops `.git`
before anything runs. Any other source with `spec.build` (a directory, a
branch or tag, an archive) installs only when every declared output is already
in the tree. That lets a release archive carry the same manifest with its
outputs prebuilt.

**Consent.** `orbit plugin add <git+url#commit> --allow-build` or
`orbit plugin upgrade <ns> <git+url#commit> --allow-build`. Without the flag
the install is refused with `build_consent_required`, and the refusal prints the
build plan:

- the source and commit;
- the `fetch` and `command` argv;
- the resolved programs and toolchain roots;
- each phase's network policy;
- the limits and outputs.

With the flag Orbit prints the same plan to stderr, then builds. Consent covers
one command. Every upgrade and every `--force` reinstall needs the flag again.
It never comes from a pin, a config key, an environment variable or an MCP
tool. A managed run (`ORBIT_RUN_ID`, `ORBIT_MANAGED_RUN_CONTEXT`), a plugin
backend child, or an Orbit agent sandbox that passes the flag is refused with
`build_consent_unavailable`. Routines, auto-tasks and jobs cannot start a
build.

**The sandbox.** Each phase runs under a deny-by-default build profile.

| | Linux: `linux-bwrap-build-v1` | macOS: `macos-sandbox-build-v1` |
|---|---|---|
| Mechanism | Bubblewrap with a constructed root | `sandbox-exec`, `(deny default)` |
| Readable | the system runtime, the declared programs and their toolchain roots, the build directory | same |
| Writable | the build directory only | same |
| Network | none in `command`; in `fetch`, TCP to port 443 only, through Landlock (ABI 4 or later) | none; no `fetch` phase |

On both platforms, credential paths under a toolchain root stay unreadable.

macOS does not run a `fetch` phase. It has no pid namespace, so a fetch process
that leaves its process group could keep its network after the phase ends. A
manifest that declares `fetch` is refused on macOS with
`build_fetch_unsupported_on_macos` before anything runs, with or without
`--allow-build`. A build with no `fetch` (vendored or offline dependencies)
runs on macOS. Otherwise, build on Linux or install a prebuilt archive.

The environment starts empty. The build receives:

- `PATH`: the programs' directories and a consented toolchain's `bin`, then
  `/usr/bin:/bin`;
- `HOME` and `TMPDIR` inside the build directory;
- `LANG=C.UTF-8`, `TZ=UTC`;
- `SOURCE_DATE_EPOCH`, set to the commit time;
- `ORBIT_BUILD_DIR`, `ORBIT_BUILD_SRC`, `ORBIT_BUILD_PHASE`;
- toolchain locators such as `RUSTUP_HOME`, only when they point inside a
  consented root.

Tokens and agent sockets (`GH_TOKEN`, `SSH_AUTH_SOCK`, `AWS_*` and similar)
are never passed.

Limits:

- each phase is killed with its whole process group at `timeout_ms`;
- the build directory is capped at 8 GiB;
- the log is capped at 1 MiB, keeping the head and the tail, and is kept at
  `~/.orbit/state/plugin-builds/<ns>/build.log`;
- core dumps are off.

The build fails, and nothing is installed, when either of these is missing:

- Bubblewrap, or Landlock ABI 4 for a `fetch` phase, on Linux;
- `sandbox-exec` on macOS.

**What is installed.** Only the declared outputs, copied from the build
directory into the tree from the verified commit. Each output must be a
regular file with no symbolic link on its path and a single hard link. It must
not replace a file the commit ships, and its mode becomes 0755 or 0644. The
install records:

- an artifact digest (`sha256:…` over each output's path, mode and hash);
- the consent, meaning when, which OS user and which Orbit version;
- the profile and the argv that ran.

A copy of this record sits beside the plugin's grant witness. A row whose build
record and witness disagree registers inactive at load.

**Pins never build.** A pin may name `source: git+<url>#<commit>` and
`artifact_digest: sha256:<hex>`; `artifact_digest` is allowed only on such a
source. `orbit plugin sync` never builds. For a missing plugin whose source
would build, sync reports the pin unsatisfied and names
`orbit plugin add <source> --allow-build`. An installed build whose commit or
artifact digest differs from the pin is reported, never rebuilt.

At
`add|upgrade --allow-build`, when this workspace pins an `artifact_digest`, a
build that produces a different digest is refused, showing both. Use a
reproducible build, or a prebuilt archive, for a pin that more than one host
must satisfy.

**Inspecting a built plugin.** `orbit plugin show <ns>` prints the build
source, commit, argv, profile, consent and artifact digest. `orbit plugin
doctor` gives each built plugin an informational row, and `orbit doctor` has a
`plugin-builds` row. Both report these findings:

- installed outputs that no longer match the recorded digest;
- a pin naming another commit or digest;
- a program or toolchain root that has gone, which blocks only the next build;
- a `.build-*` directory no running build owns.

Doctor never runs a build or contacts the source. `orbit plugin remove`
deletes the build record, its witness and the build log.
