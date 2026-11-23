# Plugins: certifying and serving

## Contents

- [Certifying a plugin for this Orbit](#certifying-a-plugin-for-this-orbit)
- [When a plugin is not serving its tools](#when-a-plugin-is-not-serving-its-tools)

## Certifying a plugin for this Orbit

```bash
orbit plugin scaffold demo            # creates ./demo/.orbit-plugin with backend, tool, panel, skill, goldens
orbit plugin scaffold demo --dir /path/to/demo # choose an explicit output directory
orbit plugin validate ./demo --render # manifest plus effective profile/env
orbit plugin test ./demo              # run its goldens through the real protocol
orbit plugin test ./demo --fixture-config ./fixture-config.json # explicit fixture keys over defaults
orbit plugin test ./demo --case status_reports_ready
orbit plugin test ./demo --update-goldens
```

`orbit plugin test` runs each `spec.tests` golden — `{name, tool, input, secrets, expect}` —
in a temp workspace and compares `expect.output` exactly, or matches the backend code in
`expect.error: {code: ...}`. String values in `input` and `expect.output` substitute
`{{workspace}}` and `{{plugin_root}}` against that hermetic run.

A case's optional
`secrets: {<name>: <value>}` supplies fixture values for declared secrets, delivered like
stored ones at version `fixture`; a declared secret the case leaves out is unset, and the
host's real secrets are never read. It exits non-zero when a
case fails, naming it. `--case <name>` runs one case without certifying the full suite;
`--update-goldens` replaces mismatched output expectations with actual output.

Configuration comes only from `spec.config.defaults`, optionally overlaid with the JSON
object in `--fixture-config <file>` (plugin keys directly, without a `plugins` or namespace
wrapper). The merged section is validated against the plugin schema before any backend
starts. String values supplied by `--fixture-config` substitute `{{workspace}}` and
`{{plugin_root}}` before they are overlaid; manifest defaults pass through unchanged, as
they do for installed calls.

The backend and `{{config.<key>}}` templates receive that
same merged section. For example, `{"root":"{{workspace}}/fixture-state","limit":2}`
in a fixture file selects disposable fixture state. Template markers inside a manifest
default remain literal and are not expanded recursively.
Caller global/workspace `[plugins.<ns>]` settings are always excluded, even with
`--workspace`, `--grant` or `--accept-requested`.

A fixture-config run does not record
certification of the installed plugin's default suite.

There is no conformance mode that executes using caller configuration. For deliberate
validation of installed settings, first use `orbit plugin validate <dir> --render` to inspect
resolved roots and the admitted environment without starting a backend; an actual installed
tool call is a separate operation using normal grants and the normal tool audit. Do not use
conformance as a live-data smoke test.

`{{workspace}}` and `{{plugin_state}}` render inside the disposable roots, but temporary
execution roots alone do **not** isolate data paths in configuration or the manifest.
Absolute paths in defaults or fixture values, explicit permissions, `env_pass`, and an
unconfined backend can still reach external state or credentials when consented. Use only
sentinel data and model stubs in disposable directories for such checks; never dispatch a
real credentialed model to reproduce an isolation defect.

The command refuses, and prints the requested grant set, when the manifest asks for an
unconfined backend (`backend.sandbox: none`), an `fs.write` root or `fs.write_files` file that
resolves outside that temp workspace, `network: any`, or any `env_pass` variable.

A write root
or file is judged where it lands after rendering with manifest defaults plus explicit fixture
overrides and resolving relative paths
against the plugin directory, so a `{{config.<key>}}` default or `../` path naming a host
directory needs consent just like an absolute one.

Re-run with `--accept-requested` to
test under that requested profile, or with `--grant` using the same names as `orbit plugin
enable --grant` (`fs`, `network`, `env_pass`, `orbit_tools`, `unsandboxed`, `agent_tools`). The `--grant`
list has to name each of those requests. Either flag applies only to this run and does not
record a host grant; `orbit plugin enable --grant` is still what authorizes the installed
plugin.

Inside another Seatbelt sandbox, such as a macOS Orbit worker's, `sandbox-exec` cannot apply the
backend's profile (exit 71, `sandbox_apply: Operation not permitted`), and the backend is never
started with weaker containment instead.

Run there as a required validation gate with
`ORBIT_VALIDATION_SUMMARY=<file>`, `ORBIT_VALIDATION_GATE='<the gate as configured>'` and
`ORBIT_VALIDATION_BASE=<delivery base>`: every case is reported `deferred` with a
`SKIP: sandbox-exec cannot apply a profile:` line, the command exits 0 and writes the summary, and
the implementer hands that summary off as a deferred sandbox validation. The claim host then reruns
the gate outside the worker sandbox, and that run is the evidence.

Without a summary, or with
`ORBIT_REQUIRE_SANDBOX_EXEC=1`, the command fails with `[validation_route_unavailable]` and runs no
case. A deferred run certifies nothing.

A passing run records this Orbit's version on the installed plugin, and `orbit plugin show <ns>` prints it as `Certified for: <version>`.
The record is only written when the directory tested is the installed one (same manifest
digest); reinstalling a changed manifest drops the claim.

`orbit plugin scaffold <namespace>` creates the plugin in `./<namespace>/.orbit-plugin/`
in the current directory; `--dir <path>` creates it in `<path>/.orbit-plugin/` instead.
`orbit plugin migrate --out-dir <dir>` likewise writes `<dir>/.orbit-plugin/plugin.yaml`. Plugin installation refuses
sources inside a workspace repository, so move the scaffold outside the repository
or give `--dir` an external path before installing it.

`orbit plugin validate <dir> --render` projects the sandbox profile and child environment
a real call would use, including the program-link directory on `PATH` when the source
declares a granted program. It does not create, repoint or remove those links, and it
starts no backend. Human and JSON inspection retain environment
names and non-sensitive values while redacting credential values; actual backend calls
still receive the admitted values.

In a registered workspace it renders for that
workspace. From an unregistered directory it uses host-level config and does not
initialize a workspace there. Pass the global `--workspace <selector>` option when
rendering needs workspace config or to inspect another registered checkout.

## When a plugin is not serving its tools

```bash
orbit plugin doctor
```

One row per plugin, naming the step that would make it active, plus a row for
any skill link whose target has gone and one for each declared secret that is
not set (see [Secrets](plugins-permissions.md#secrets)). The four states:

| Status | What it means |
|---|---|
| `active` | Installed, enabled, and serving its tools. |
| `disabled` | Installed but not enabled on the host — run `orbit plugin enable <ns>`; or switched off in this workspace — `orbit plugin enable <ns> --scope workspace`. `list` and `show` print the host state and which one it is. `doctor` does not count a workspace toggle as a problem. |
| `missing` | Pinned by this checkout, not installed here — run `orbit plugin sync`. |
| `inactive` | Enabled but refused at load: the manifest no longer loads, its `requires.orbit`/`requires.host_api` does not hold on this machine, its namespace collides, a definition it ships breaks the rules above, or its `[plugins.<ns>]` config fails its own schema. `orbit plugin show <ns>` names the reason. |

A plugin fails closed on its own: one broken plugin never takes down the
built-in tools, the runtime, or another plugin.
