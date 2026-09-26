---
type: design
title: "Design: host-side broker for agent-initiated plugin calls"
summary: "Design: a per-run host broker runs plugin backends for sandboxed agents, so agent sandboxes can be denied state/plugins and the plugin secret store"
owner: claude
status: Draft
tags: [plugins, security, sandbox, secrets, ipc]
paths: ["crates/orbit-core/src/adapter/engine_host/v2_host/sandbox.rs", "crates/orbit-exec/src/linux_sandbox/**", "crates/orbit-exec/src/macos_sandbox/**", "crates/orbit-core/src/runtime/plugin/**", "crates/orbit-tools/src/plugin/backend/**"]
related_features: [policy-sandbox, plugins]
related_artifacts: [ORB-13038, ORB-13008, ORB-13009, F2026-09-230]
last_updated: 2026-09-26
last_validated: 2026-09-26
---

# Design: host-side broker for agent-initiated plugin calls

Status: proposal. Nothing here is implemented; the follow-up tasks listed at the end deliver it.
Builds on [1_scope.md](./1_scope.md) §3 ("Plugin secrets") and §4.2–§4.3, and on the agent
sandbox described in [policy-sandbox 2_design.md §7](../policy-sandbox/2_design.md#7-sandbox--exec-primitives).

A plugin backend holds credentials for the service it wraps, either in its own
`{{plugin_state}}` or in the host secret store. Today any sandboxed agent worker can read both,
because the agent sandbox leaves the global root readable for the nested `orbit` its tool calls
run. This note moves agent-initiated plugin calls out of the agent sandbox. The nested `orbit`
forwards the call over an authenticated Unix socket to a broker in the host process that
dispatched the agent. The broker spawns the confined backend, so the agent sandbox can then
deny `state/plugins/` and `state/plugin-secrets/` outright.

## 1. Threat model

**Asset.** A plugin's durable credentials: the files under `<global_root>/state/plugins/<ns>/`
(`{{plugin_state}}`) and the plugin's file in `<global_root>/state/plugin-secrets/`. pulsar's X
OAuth refresh token is the first real case. Anyone holding it can post as the account until the
operator revokes it.

**Adversary.** A CLI agent worker (claude, codex, gemini, grok) that Orbit dispatched under an
agent sandbox (Bubblewrap on Linux, `sandbox-exec` on macOS) and that is hostile. It has
been prompt-injected, or it simply misbehaves. It runs as the operator's UID, has a shell and
any program the host has, sets its own environment, and may call every tool its activity
allowlist exposes, plugin tools included.

**What the design guarantees.** Such an agent can *use* a plugin tool it is allowed to call, and
receives that tool's output. It cannot read a plugin's state or secret bytes, directly or
through any process running inside its sandbox. It cannot make a backend run with a caller
identity, workspace or tool ceiling other than the one Orbit dispatched it with.

**Out of scope.**

- Same-UID processes outside any Orbit sandbox: the operator's shell, the dashboard, an
  executor configured `sandbox: off` or running a bare `allow_fallback`. They can already read
  every file the operator can. The broker still serves them where it exists, but makes no
  confidentiality claim about them.
- Root, the kernel, and a host that mounts the global root at a second path the agent sandbox
  does not mask (§6.4).
- A backend that leaks its own credential in its tool output. The output reaches the caller by
  design, so keeping credentials out of it is the plugin's job.
- Which tools an agent may call. Activity allowlists and `permissions.orbit_tools` are
  unchanged.

## 2. Why a direct deny in the agent sandbox is not enough

ORB-13008 first asked for `state/plugins/` to be added to the agent sandbox's read denies. Its
worker showed that this breaks every plugin call an agent makes [F2026-09-230]. Every process an
agent starts inherits the agent's restrictions, and neither platform lets a descendant widen
them again:

- **Linux.** The agent runs in a Bubblewrap mount namespace (`--unshare-all --share-net
  --ro-bind / /`, `crates/orbit-exec/src/linux_sandbox/argv.rs`). A deny for a directory has to
  be a mount over it, and every descendant sees the same mount table. A nested `orbit` spawns
  the plugin backend under its own Landlock ruleset (`spawn_under_linux_landlock_boundary`), but
  Landlock can only take access away. It cannot unmask a path hidden by the surrounding mount.
  Any nested user namespace the agent creates receives that mount locked, so it cannot unmount
  it either.
- **macOS.** A `sandbox-exec` profile applies to the process and all its descendants. The
  backend's own plugin profile is checked in addition to the agent's, never in place of it, so
  a `(deny file-read* (subpath …/state/plugins))` in the agent profile also denies the
  backend's re-allow of its own `{{plugin_state}}`.

So the process that needs the credential (the backend) and the process that must not reach it
(the agent) cannot both be descendants of the same sandboxed process. At least one of them has
to be spawned from outside the agent sandbox.

The in-sandbox path has two more defects today, even before any deny is added. Neither agent
profile grants writes to `state/plugins/` or `state/plugin-secrets/`
(`append_linux_runtime_write_roots` and `append_orbit_child_runtime_write_roots` in
`crates/orbit-core/src/adapter/engine_host/v2_host/sandbox.rs`). So an agent-initiated call
cannot:

- create a missing `{{plugin_state}}` or write to it, even when the backend holds an `fs.write`
  grant there;
- take the secret store's per-plugin lock file. Every `secret_updates` rotation from such a
  call is therefore refused. With X, which invalidates the old refresh token when it issues a
  new one, a refused rotation loses the account's only valid token.

This follows from the code and has not been measured on a live host. The broker fixes both
defects, because the backend and the store writes then run on the host.

### Alternatives considered

- **Envelope-only delivery with a host-only store.** Keep spawning the backend inside the
  sandbox, and deliver secrets in `context.secrets` (as ORB-13009 does) from a store the
  sandbox cannot read. This fails for two reasons. First, something inside the sandbox still
  has to read the store to fill in the envelope. Second, the backend still needs
  `{{plugin_state}}` readable inside the sandbox, so the same deny breaks it. The value would
  also pass through the nested `orbit`, whose memory and pipes an ancestor in the agent's
  process tree can inspect.
- **A host daemon (`orbit web serve`, a system service).** Dispatch does not require a daemon:
  a drain, `orbit run ship` or a clock tick run agents without one. A shared daemon would also
  have to work out which run a connection belongs to, and it would be one process serving
  every run's calls. A broker scoped to one run already knows the run's identity, workspace,
  allowlist and filesystem profile, and it lives exactly as long as the agent does.
- **Descriptor inheritance (a `socketpair` passed like `ORBIT_PLUGIN_CALLBACK_FD`).** Orbit
  spawns a plugin backend directly, so it controls which descriptors the backend inherits. An
  agent's nested `orbit` is started by a third-party CLI and its tool runner, and each of them
  decides whether descriptors survive. The broker is reached through a filesystem path instead,
  and the kernel authenticates the peer (§4).

## 3. The boundary

```text
agent sandbox (bwrap / sandbox-exec)            host: run-pipeline-worker process
+----------------------------------------+      +--------------------------------------+
| claude / codex                         |      | run broker (one per agent run)       |
|   -> orbit tool run pulsar.post        | UDS  |   authenticate peer -> this run      |
|   -> orbit mcp serve (tools/call)  ----+----->|   authorize with the run's allowlist |
|                                        |      |   read secrets, audit                |
| state/plugins/, state/plugin-secrets/: |      |   spawn backend -------------------->+--> plugin backend
|   masked (sentinel only, no bytes)     |      |                                      |    (plugin profile
+----------------------------------------+      +--------------------------------------+     ∩ agent profile, §5)
```

**Rule.** Inside an agent sandbox that has a broker, a plugin tool call is never run in-process.
The nested `orbit` sends it to the broker, and the broker is the only party that reads the
plugin's secrets or spawns its backend. Built-in `orbit.*` tools are unchanged and still run in
the nested `orbit`.

**Where the broker lives.** It runs in the `orbit job run-pipeline-worker` process that
executes the agent step. `run_cli_backend`
(`crates/orbit-engine/src/activity_job/cli_runner/orchestrator.rs`) spawns the sandboxed
provider through `spawn_child_with_optional_sandbox` (`cli_runner/spawn.rs`), records the child
PID and, on Linux, its Bubblewrap PID namespace, and then blocks in `spawn_with_timeout` until
the provider exits. That process is outside the sandbox and stays alive for the whole agent
run. It already holds the run's authoritative context: run ID, task ID, workspace, the
activity's tool allowlist (`ORBIT_ACTIVITY_TOOLS`), and the resolved filesystem profile the
agent was sandboxed with. Nothing in Orbit listens on a local socket today: the only listeners
are the TCP ones behind `orbit web serve` and `orbit mcp listen`. So there is no existing
authenticated channel to reuse, and the broker is new.

The broker is a bounded listener thread owned by that process. It starts before the agent is
spawned, and when the agent exits it is torn down together with any in-flight backend process
groups. A failure inside the broker never brings down the step runner. Plugin dispatch lives
behind `orbit-core`, not `orbit-engine`, so the listener reaches dispatch through the engine's
existing host seam (`RuntimeHost`) and not through a new crate dependency.

**Which calls it serves.** Every call a sandboxed nested `orbit` makes to a tool whose
registration is a plugin backend. That covers `orbit tool run <ns>.<verb>`, its
`orbit <ns> <verb>` spelling, and `tools/call` on an agent's `orbit mcp serve`. Tool listing,
schemas and `--help` still come from the nested `orbit`. Those need only the plugin rows,
install trees and grant witnesses, which stay readable. Calls that are not made from inside an
agent sandbox keep their current path: job steps (`plugin.tool_call`), clock ticks, the
dashboard, host MCP servers, and an operator's shell.

## 4. IPC and authentication

### 4.1 Socket and discovery

- The host binds a `SOCK_STREAM` Unix socket at `<global_root>/state/plugin-broker/<token>.sock`.
  `<token>` is 16 random hex characters, fresh per run, and serves only to avoid collisions; it
  is not a credential. The directory is host-owned and mode `0700`, and it is not in any agent
  profile's write inventory, so no agent can replace or unlink another run's socket. The host
  refuses a directory that is a symlink.
- The socket path is exported to the agent as `ORBIT_PLUGIN_BROKER=<path>`, next to the existing
  managed-run envelope names (`ORBIT_RUN_ID`, `ORBIT_MANAGED_RUN_CONTEXT`, …). The value tells
  the client where to connect and proves nothing: authentication is the kernel's peer identity
  below.
- A path longer than `sun_path` allows (108 bytes on Linux, 104 on macOS) is not shortened by
  moving it to `/tmp`. Bubblewrap gives the agent a private `/tmp`, and macOS agents can write
  to the shared temporary roots. The broker is reported unavailable instead (§7). Standard
  global roots (`~/.orbit`) are far below the limit.
- The client checks that the server runs as its own UID (`SO_PEERCRED` / `getpeereid`) before
  it sends anything.

### 4.2 Peer authentication

The broker authenticates each **connection**, not a secret the client presents. The agent can
read everything in its own environment and, on macOS, the environment of other same-user
processes too, so a bearer token would authenticate nothing.

- **Linux.** Bubblewrap gives every agent run its own PID namespace (`--unshare-all`). When the
  host spawns the agent it already records that namespace (`bind_worker_namespace` in
  `crates/orbit-core/src/runtime/recovery_authority.rs`). On accept, the broker reads the peer's
  UID and host-namespace PID (`SO_PEERCRED`; `SO_PEERPIDFD` where the kernel has it, so a
  recycled PID cannot be substituted). It accepts the connection only if the UID is its own and
  `/proc/<pid>/ns/pid` is the run's recorded namespace, with the leader's start time and boot
  ID matching as `namespace_key` already checks. A process in another run's sandbox is in a
  different PID namespace and cannot join this one. Being able to see the socket path, which
  the read-only bind of `/` allows, is therefore not enough to use it.
- **macOS.** There are no PID namespaces. The broker reads the peer's audit token
  (`LOCAL_PEERTOKEN`, falling back to `LOCAL_PEERPID`) and walks its parent chain with
  `proc_pidinfo(PROC_PIDTBSDINFO)`, checking each PID's start time, until it reaches the
  `sandbox-exec` process it spawned for this run. A peer that is not a descendant of that
  process is refused. This includes an orphan reparented to `launchd`, so a tool call made from a
  daemonized grandchild fails closed.
- In both cases the broker checks the peer again after reading the request and before spawning
  the backend. A peer that exited in between is refused.

A refused connection is closed with no reply, and the refusal is logged with the peer PID and
the reason. Nothing reveals whether the socket belongs to a live run.

### 4.3 Protocol

Length-prefixed JSON frames. There is one request and one response per connection, a 4 MiB
request cap, and the response is capped at the host's existing tool-output limits.

```text
request:  {"schema_version":1,"tool":"pulsar.post","input":{…},"cwd":"…","workspace":…|null,
           "entry_point":"cli"|"mcp","dry_run":false}
response: {"schema_version":1,"ok":true,"output":{…}}
          {"schema_version":1,"ok":false,"error":{"code":…,"message":…,"retryable":…,"detail":…}}
```

The request carries only what the caller legitimately chooses: the tool, its input, a `cwd` the
broker requires to lie within the run's worktree, and an optional workspace selector the broker
resolves against the run's own registry. Everything that decides authority comes from the
broker's own dispatch record and is never read from the request or the client's environment:
run ID, task ID, the activity's allowlist, the filesystem profile and the tool ceiling.
Consequently `context.task_id` and `context.job_run_id` (1_scope.md §4.2 "Call identity")
become authenticated for brokered calls, where today they are host-attested only.

`secret_updates` never leave the host. The broker applies them with the store's compare-and-swap
exactly as the in-process host does and returns only `ok`/`output`/`error`. `entry_point` is a
hint for the audit row; it grants nothing.

### 4.4 Execution and audit

The broker runs the call through the same audited dispatch the in-process path uses. The audit
row carries the usual plugin fields plus `brokered: true` and the peer PID. The nested `orbit`
writes no dispatch row of its own for a forwarded call, so each call is counted once. `mcp`
backends are kept per caller context inside the broker, as §4.2 of the scope describes for any
runtime, and are reclaimed when the run ends.

**Limits.** A broker runs at most four calls at once and queues at most sixteen more. Anything
beyond that is refused with `plugin_broker_busy`, `retryable: true`. Each call's deadline is
the backend's `timeout_ms`, capped at `PLUGIN_TIMEOUT_CEILING_MS`. The client waits for that
deadline plus a fixed grace. If the client disconnects, the broker kills the backend's process
group. A rotation the backend already reported is still applied, because it happened on the
service side regardless.

## 5. Confinement of a brokered backend

Once the backend moves to the host, the agent's sandbox no longer applies to it by inheritance.
Nothing else may widen: whatever the backend could do when it ran inside the agent sandbox is
still all it can do, with one exception. So the broker compiles the backend's profile from
two inputs:

- **The plugin profile**, exactly as §4.3 of the scope defines it: granted `fs`, `network`,
  `programs`, the unreadable trees, and the re-allow of the plugin's own `{{plugin_state}}`.
- **The calling run's resolved agent profile.**
  - Every plugin write root must also be writable under the agent profile; a root that is not
    is dropped with a diagnostic, the same way §4.1 admission already refuses one.
  - Every agent `denyRead` rule, and the default credential denies, is added to the backend's
    read denies.
  - `{{workspace}}` renders to the run's worktree, the same root the in-sandbox call would have
    had.
  - The backend gets the run's `proc.spawn` allowlist as its restricted caller allowlist.

The single intended widening is `{{plugin_state}}`. The backend can read it and, with an
`fs.write` grant, write it, even though the agent profile masks `state/plugins/`. The secret
store is not widened for anyone: secrets reach the backend only in the call envelope.

On Linux the backend still runs under Landlock with no Bubblewrap wrapper
([policy-sandbox 2_design.md §7.3](../policy-sandbox/2_design.md#the-plugin-backend-boundary)),
so the write intersection is computed when the ruleset is compiled. On macOS the intersection
is compiled into one SBPL profile. The compiler stays `compile_macos_sandbox_profile`, fed the
intersected roots.

A backend that runs unsandboxed (`backend.sandbox: none`, `unsandboxed` grant) runs on the host
without restrictions, as it does from a clock tick. The operator already accepted that when
granting `unsandboxed`. `orbit plugin doctor` notes that agents can reach it.

## 6. Denying the trees to the agent sandbox

### 6.1 What is denied

`<global_root>/state/plugins/` and `<global_root>/state/plugin-secrets/`, for reads and writes,
on both platforms. Other nested-`orbit` reads under the global root are unchanged. That
includes `plugins/`, `plugins/.grants/` (the loader verifies witnesses when it builds the tool
surface) and `state/plugin-callbacks/`.

### 6.2 Mechanics

- **Linux (Bubblewrap).** The host creates both directories if they are missing (mode `0700`)
  and a read-only sentinel directory `<global_root>/state/plugin-broker/masked/` that holds
  one file, `.orbit-brokered`. The Bubblewrap plan then adds `--ro-bind <sentinel> <tree>` for
  each tree. It does this after every policy mount, so no earlier grant can expose the real
  directory. The trees are masked for the agent and every descendant, including
  `proc.spawn` children under Landlock. Mounts that Bubblewrap creates cannot be undone by a
  process that has no capabilities, and a nested user namespace receives them locked.
- **macOS (`sandbox-exec`).** The agent profile gets `(deny file-read* file-write*
  (subpath <tree>))` for both trees, compiled from their physical paths (the same
  `/private/var` rule the plugin profile uses) after every allow, so the deny is the last
  match.

### 6.3 Nested `orbit` behaviour inside a masked sandbox

A broker-capable host masks every agent it sandboxes, and it exports `ORBIT_PLUGIN_BROKER` only
when that run's broker actually bound (§7). A broker that fails to bind therefore costs the run
its plugin calls, never the mask. A nested `orbit` detects that it is in a masked sandbox from
the sentinel on Linux, or from a permission error on the tree on macOS, *before* it spawns
anything:

- With `ORBIT_PLUGIN_BROKER` set, plugin tool calls go to the broker.
- Without it (the agent unset it, or a wrapper removed it), plugin tool calls are refused with
  `plugin_broker_unavailable`. The nested `orbit` never falls back to in-process execution:
  the mask hides the backend's state, and the secret store reads as empty.
- The secret store never treats a sentinel directory or a permission error as "no secrets
  set". Today `PluginSecretStore::read` maps only `NotFound` to an empty file. The masked
  directory must refuse, not read as an empty directory, because an empty `context.secrets`
  would send a backend into its "not configured" path.
- Operator commands that read the trees (`orbit plugin secret list`, `plugin show` secret
  state, `plugin doctor` state and secret checks, `plugin remove --purge-state`) report
  "not visible from an agent sandbox" and change nothing. These were never agent tasks:
  lifecycle verbs already need writes to `plugins/`, which the agent profile does not grant.

### 6.4 Honest limits

- The mask works on paths. If the global root is also reachable through a second mount of
  the same filesystem, or through a bind mount outside the global root, that second path is
  not masked. The Linux implementation compares the device and inode of both trees against
  every other mount the plan exposes and refuses to start the agent with the mask incomplete;
  macOS matches `subpath` on the physical path.
- On macOS the guarantee also depends on the agent profile not granting another process's task
  port. The broker and the backend are not the agent's descendants, but they do run as the same
  user. The implementation proves with a test that a process inside the agent profile cannot
  read the broker's or the backend's memory.
- An agent can still use a plugin however its allowlist lets it: post with the account, for
  example. Taking a credential and using a tool are separated. Tool authorization is not
  tightened here.
- `state/plugin-callbacks/` stays readable to agents, and that is not incidental. A nested
  `orbit` that cannot list it concludes it is inside a plugin sandbox and refuses itself
  (1_scope.md §4.2, "A confined child with no credential is refused"). Code reading suggests
  that an agent could open a live callback record and present it on descriptor 3 as a
  backend's credential. That has not been tested. It is a separate question from state and
  secret confidentiality, and this design does not change it.

## 7. When the broker is unavailable

| Situation | Behaviour |
|---|---|
| The host cannot create the directory or bind the socket (path too long, `EACCES`, disk full) | The step still runs, with the mask applied and no `ORBIT_PLUGIN_BROKER`. Plugin calls fail with `plugin_broker_unavailable`, `retryable: false`, naming the cause. A broker failure costs plugin calls, never confidentiality. |
| The executor runs unsandboxed (`sandbox: off`, bare fallback) | No mask and no broker. Plugin calls run in-process as they do today; the §1 non-goal covers this. |
| The socket is gone or refuses the connection mid-run (step runner crashed or is shutting down) | `plugin_broker_unavailable`, `retryable: false`. The agent is being torn down anyway (`--die-with-parent`). |
| The broker is at its concurrency limit | `plugin_broker_busy`, `retryable: true`. |
| Peer authentication fails | The connection is closed with no reply. The client reports `plugin_broker_unavailable` and the host logs the refusal. |
| The client disconnects mid-call | The backend's process group is killed. A reported rotation is still applied. |
| The host is an older Orbit that starts no broker | It applies no mask either, so nested calls keep today's in-process path. Rollout order (§8) keeps this pairing. |

Every refusal reaches the agent as an ordinary structured plugin error (1_scope.md §4.2), and
`orbit tool run` exits non-zero with the JSON on stderr, as it does for any other plugin error.

## 8. Rollout and follow-up tasks

The mask ships last, only once every call it would break has a broker to go to:

1. **Broker server and peer authentication.** The per-run listener in the CLI step runner, the
   wire protocol, Linux PID-namespace and macOS ancestry authentication, limits, teardown, and
   `ORBIT_PLUGIN_BROKER` export. It serves nothing until the client exists. [ORB-13236]
2. **Brokered backend confinement.** Compile the plugin profile against the calling run's
   agent profile (§5) on both platforms, with `{{plugin_state}}` as the only widening. This is a
   pure profile-compilation change, testable without the broker. [ORB-13237]
3. **Nested client forwarding.** `orbit tool run`, the `orbit <ns> <verb>` group and
   `orbit mcp serve` forward plugin calls when `ORBIT_PLUGIN_BROKER` is set. The broker runs
   them through the audited dispatch with the authoritative run context and the §5 profile.
   This slice also adds `brokered` audit fields and the §7 error codes, and updates
   1_scope.md §4.2 "Call identity". Blocked by 1 and 2. [ORB-13238]
4. **Agent sandbox mask.** The §6 mask on Linux and macOS, applied to every sandboxed agent
   whether or not its broker bound, plus the §6.3 nested behaviour: the sentinel, no in-process fallback, the secret
   store refusing a masked directory, and operator commands degrading. This slice also updates
   1_scope.md §3 and §4.3 to remove the "agent sandboxes can still read it" gap. Blocked by 3.
   [ORB-13239]

## Task References

- [ORB-13038] — this design.
- [ORB-13008] — plugin-vs-plugin isolation of `state/plugins/<ns>`; its agent-side deny was
  split out to this design.
- [ORB-13009] — the host secret store and per-call delivery the broker reuses.
- [ORB-13236], [ORB-13237], [ORB-13238], [ORB-13239] — the implementation slices in §8.

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
