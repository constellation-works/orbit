# MCP Apps compatibility prototype

This candidate serves `ui://orbit/task-panel/v1/index.html` as
`text/html;profile=mcp-app`. `orbit_ui_open` advertises a global entrypoint;
`orbit_ui_inspect` advertises a thread entrypoint. Both are read-only. An empty
open shows the selection form; a selected open or inspect requires both an
explicit workspace selector and public task key. The panel refreshes through
`orbit_task_show`, never through dashboard HTTP. Existing data tools have no
widget metadata and retain their schemas and dispatch behavior.

The adapter owns resource discovery, a static bundle, and presentation
metadata. It requires the host to expose the annotated read-only task reader,
then delegates selected reads to that host's `orbit.task.show`, including the
explicit workspace filter and a six-field projection. Core still owns policy,
validation, audit and workspace resolution (STD-02 §R2, STD-05 §R1). No shared
contract or dependency edge was added. The manifest requires rmcp 2.1.0; the
current lockfile resolves 2.2.0 and supplies the necessary hooks.

Task text is displayed with `textContent`, including Markdown as plain text.
The bundle has no external assets, fetches, frames, artifact execution or
navigation. Both resource CSP metadata and a restrictive HTML CSP limit it.
Resource reads accept only the exact versioned URI, without query parameters,
path normalization or remote forwarding. Titles, description and criteria are
bounded in the display; a context reference includes only identity, observed
read time, returned task update time and a bounded title. Update time is an
observation, not an atomic revision token. The resource URI must change for a
breaking bundle change.

The bridge negotiates MCP Apps `2026-01-26`, then requires `serverTools`.
Unsupported initialization disables reads. `updateModelContext` is detected
before the explicit **Send context to chat** action; absent or refused support
leaves a selectable copy reference. It never sends context automatically.
Failed reads, changed selections, mismatched responses, timeouts and teardown
invalidate context; late responses cannot retarget a selection. Last displayed
text remains visibly stale on failure. Context remains untrusted discussion
material: the consumer must reread state before any action. No actor claim,
UI metadata or click supplies operator capability. This prototype has no task
writes, dispatch buttons or completion controls.

## Automated reproduction

Use the run's existing build directory. Before allocating a new build directory
or checkout, run `df --output=pcent <parent-directory>` and stop at 80% usage.
No installation, release or global configuration changes are needed:

```sh
./scripts/build-budget.py -- cargo test -p orbit-mcp --test mcp_wire_roundtrip
./scripts/build-budget.py -- cargo test -p orbit-cli --test mcp_roundtrip mcp_apps_
node --test crates/orbit-mcp/src/adapter/tests/task-panel.mjs
make ci-fast
make ci-lint
make goldens
python3 scripts/probe-mcp-apps.py --binary target/debug/orbit
ORBIT_PANEL_RESOURCE=.orbit/tmp/mcp-apps-probe/task-panel.html node --test crates/orbit-mcp/src/adapter/tests/task-panel.mjs
```

The wire fixtures run the real MCP server and the production stdio dispatcher
against isolated state. The JavaScript fixture executes the shipped script in
a deterministic host/DOM harness, proving text sinks, bridge calls, freshness
and refusal behavior. It does not exercise a browser engine or desktop shell.
Node is needed for this explicit bridge check; it is not a Rust runtime dependency.
The probe requires Python 3.9+, Git and an already-built candidate binary.
It uses a cleared child environment with a disposable HOME, verifies checkout
routing before task creation, captures JSON-RPC, and stages a minimal
`orbit-probe` package whose launcher addresses that exact fixture and binary.
It never installs that package. A failing probe exits nonzero; inspect its
transcript rather than accepting partially written evidence. Repeating it
requires a fresh `--output .orbit/tmp/<unique-name>` directory. Keep probe data
until the native handoff is complete.

## Native desktop handoff

**Native checks are NOT RUN on the Linux execution host.** An automated PASS
means only the named protocol or JavaScript check passed. The main-build gate
remains `PENDING_ACTUAL_HOST_PROOF`; this prototype can be reviewed before that
gate. Absence of desktop access does not establish unsupported behavior.

1. On the actual desktop host, check out this candidate and build using its
   existing build location. Run the reproduction commands, then the probe.
   Keep the emitted workspace, public task key, plugin path and evidence path.
   Use the emitted candidate HEAD and content digest, which includes untracked
   delivery files, to identify an uncommitted candidate.
   Copy `evidence.json` for the native observations using the fields in
   [the evidence template](mcp-apps-evidence-template.json).
2. Record the exact desktop product/build/OS, backend binary/version, candidate
   revision, plugin source and installed path, selected registration and its
   command/args. Inventory both manual `mcp_servers.orbit` and `orbit@orbit`
   plugin registrations. They may coexist; do not assume precedence. Use the
   distinct `orbit-probe` candidate for the test and confirm the observed server
   launch points to `serve-candidate.py`. A production registration that resolves
   to `npx @orbit-tools/cli@0.25.1` is not this candidate.
3. In a disposable desktop profile, open the emitted fixture workspace. It
   contains a repo marketplace at `.agents/plugins/marketplace.json` with
   `orbit-probe` marked AVAILABLE. Restart the desktop app, find the local
   candidate in its Plugins directory and install/select it for that profile.
   The probe stages this marketplace; it never performs installation or enablement.
   Record the actual installed copy and confirm its launcher matches the source.
   If the client cannot load local stdio UI packages, record the exact observed
   refusal/build and leave subsequent scenarios NOT RUN. Do not change the
   user's normal registrations or expose a public HTTP service. The loading
   control is client-specific; lack of one is evidence to record, not a reason
   to guess a configuration key.
4. Open the global sidebar entry, enter the emitted workspace and task key,
   and read. Record resource discovery, rendering and the exact displayed
   identity. Open the same entity via `orbit_ui_inspect` in a conversation and
   record thread-panel behavior independently. Confirm the malicious-looking
   fixture title remains literal text and no script or external request runs.
5. Click **Send context to chat**, inspect what the conversation receives, and
   record supported bridge behavior or the copy-reference fallback. Change the
   workspace/key or force a read failure; confirm context is disabled and the
   old display says stale. Verify a governed action remains refused in this
   unprivileged fixture. Never treat host context as proof of human authority.
6. Repeat each authorized connection mode separately using disposable state on
   each accepting host. Record the selected registration/argv, backend version,
   opaque returned workspace selector and result. Preserve same-named workspace
   distinctions. An unknown/offline/wrong selector must refuse, with no local
   fallback. Capture the visible error and the authoritative destination's log.
7. Fill every native outcome with PASS, FAIL, UNSUPPORTED (only after an observed
   refusal) or NOT RUN plus the reason. Attach screenshots/recording and the
   exact server transcript. Obtain the separate actual-host go decision before
   beginning the main product build. Loading or inspecting this candidate is
   not permission to merge, release or replace a live binary.

## Connection-mode boundaries

| Mode | Resource owner | Data routing and current evidence boundary |
| --- | --- | --- |
| Local stdio | Accepting adapter | Production isolated fixture; explicit filter enforced. Native rendering requires the desktop probe. |
| Direct SSH byte relay | Destination adapter | Existing non-PTY proxy inherits stdio without translating resources. The destination must run this candidate to advertise its UI; an older destination's absence/refusal must be recorded. Native and real SSH checks require prepared hosts. |
| Federated local | Accepting mux adapter | Static resources stay local; task reads use the existing qualified-selector route. Covered by the production fixture. |
| Federated remote | Accepting mux adapter | Only `orbit.task.show` is forwarded, with existing identity pinning and advertised-tool checks. The destination needs that data contract, not resource forwarding. Missing tool, stale route, offline or wrong-host selection retains the mux error. Native and real remote checks require prepared hosts. |

For connection-mode probes, record these candidate argv separately:
`mcp serve` for local, `mcp serve --mode remote <fixture-ssh-host>` for direct
SSH, and `mcp serve --mode federated` for the mux. The staged launcher is local;
prepare separate disposable launchers for other modes and retain their exact
argv/environment in the evidence. The direct relay invokes `orbit` on the
SSH destination's PATH, so an authorized disposable destination must resolve
that name to the candidate without replacing a production binary. A local
candidate cannot add resource support to an older remote adapter. For federation,
configure only the accepting fixture's `.orbit/mcp-destinations.toml`, pin each
destination machine identity, and copy the returned qualified selector. Never
modify the normal HOME's destination catalog for this probe. Real remote modes
remain NOT RUN until such destinations are available.

The spike does not prove long-lived upgrade handling, a production control
center, secure human-action provenance, or a full browser accessibility pass.
The existing proxy/mux tests supply routing evidence; protocol feasibility is
not an actual-host compatibility claim.

Implementation metadata and bridge methods follow the official
[UI guide](https://developers.openai.com/plugins/build/chatgpt-ui),
[quickstart](https://developers.openai.com/plugins/build/app-quickstart) and
[entrypoint guide](https://developers.openai.com/plugins/build/extensions).
The [packaging guide](https://developers.openai.com/plugins/build/plugins)
describes local/repository development packages. Those contracts inform this
candidate; they do not prove feature availability in an installed client.
