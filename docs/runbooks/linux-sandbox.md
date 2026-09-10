---
type: runbook
summary: Install and verify the Bubblewrap host prerequisite that Orbit's Linux sandbox fails closed without.
tags: [operations, sandbox, linux, bubblewrap, apparmor]
paths: ["crates/orbit-exec/src/**"]
related_features: [policy-sandbox, executors]
related_artifacts: []
last_validated: 2026-09-03
---

# Prepare a Linux Host for Sandboxed Dispatch

Use this runbook after `orbit init` on a Linux host, before dispatching any agent, or when a
run fails with `bwrap: setting up uid map: Permission denied`.

## Why the probe fails closed

`orbit init` persists the host-appropriate sandbox into the shipped executor artifacts. On
Linux that value is `linux-bwrap`, which resolves the trusted wrapper at `/usr/bin/bwrap` and
fails closed if its namespace-and-mount capability probe cannot run. Ubuntu 24.04 (Noble) also
enables AppArmor restrictions on unprivileged user namespaces; without the distro's narrow
Bubblewrap profile the probe fails with the UID-map error above.

The Linux boundary enforces writes from the resolved policy. It leaves host filesystem reads
and host network access available, so it does not provide worktree-only reads or policy-gated
network egress. See [policy-sandbox](../design/policy-sandbox/) for the design.

## Install and verify on Ubuntu 24.04

The package ships `bwrap-userns-restrict` under `/usr/share/apparmor/extra-profiles/`. Copy
that narrow profile into `/etc/apparmor.d/`, load it, confirm AppArmor knows it, then run the
same capability shape Orbit probes:

```bash
sudo apt-get update
sudo apt-get install --yes bubblewrap apparmor-profiles
test -x /usr/bin/bwrap
test -f /usr/share/apparmor/extra-profiles/bwrap-userns-restrict
sudo install -m 0644 \
  /usr/share/apparmor/extra-profiles/bwrap-userns-restrict \
  /etc/apparmor.d/bwrap-userns-restrict
test -f /etc/apparmor.d/bwrap-userns-restrict
sudo apparmor_parser -r /etc/apparmor.d/bwrap-userns-restrict
grep -Fq 'bwrap-userns-restrict' /sys/kernel/security/apparmor/profiles

/usr/bin/bwrap \
  --die-with-parent \
  --new-session \
  --unshare-all \
  --share-net \
  --ro-bind / / \
  -- /bin/true
```

The final command must exit successfully.

## If the probe still fails

Do not disable `kernel.apparmor_restrict_unprivileged_userns` globally and do not enable
`allow_fallback`; both weaken or bypass the fail-closed boundary. Re-check the packaged
profile path and the `apparmor_parser` output, then rerun the probe.

Other distributions need the same two conditions: an executable `/usr/bin/bwrap` and
permission for an unprivileged user to create user namespaces and mounts. Non-Linux hosts
are unaffected: macOS uses `sandbox-exec`, and platforms without a shipped OS-level backend
rely on in-process filesystem guards only.

## Explicitly disable worker sandboxing

An operator can persistently disable sandboxing for an executor by setting
`spec.sandbox: off` in its host-global resource, normally
`~/.orbit/resources/executors/codex.yaml` (use the corresponding filename for
another provider).

Before editing shared executor YAML, complete the mixed-version rollout below.
Replacing the CLI binary does not update an already-running MCP server or job
runner. Preserve the rest of the resource when changing the sandbox field:

```yaml
spec:
  # Keep the executor's existing command, arguments, and other settings.
  sandbox: off
```

This setting applies to future worker launches using that executor across
workspaces on the host. It survives fresh runtime opens, ordinary `orbit init`
(without `--force`), repeated default seeding, and normal `orbit workspace
sync`. `orbit init --force` resets the global root to shipped defaults,
including executor sandbox, and therefore restores the sandboxed shipped
value. Existing processes keep their launch configuration.

`off` is distinct from an omitted or `null` sandbox field. Omitted/null values
on installed Linux defaults are legacy unspecified settings and migrate to
`linux-bwrap`; deleting the field is therefore not a persistent opt-out.
Concrete backend names keep their platform validation. Invalid spellings such
as `disabled` or boolean `false` fail resource parsing.

With `off`, Orbit does not probe or launch Bubblewrap or `sandbox-exec`, and it
neutralizes the supported provider-inner sandbox flags (including Codex's
`--sandbox danger-full-access`). The workspace `execution.codex.sandbox` key
alone controls Codex's inner mode, not Orbit's outer wrapper. Explicit executor
`off` takes precedence over that inner mode. Tool authorization and policy
checks still apply to Orbit tool calls; provider subprocess filesystem access
has no Orbit sandbox confinement.

Inspect the configured choice with `orbit executor show codex --json` (the
`sandbox` field is `"off"`). The invocation audit reports `sandbox_backend: off`,
no trusted wrapper or probe, and `write_unrestricted` / `read_unrestricted`.
This is an operator opt-out, not a fallback after a failed security check, and
successful bare execution does not establish Bubblewrap or AppArmor enforcement.

### Mixed-version rollout and rollback

Older Orbit readers accept only the concrete backend enum values. They reject
`off` with `spec.sandbox: unknown variant off`; retaining `schemaVersion: 2`
does not make this new value readable by those binaries. New Orbit versions
continue to read existing resources, but backward reading of `off` by an old
version is unsupported. Executor loading during bootstrap/seeding or dispatch
can therefore break ordinary operations before they reach their requested tool.

Use this order on every process sharing the host resource directory:

1. Keep the existing executor YAML unchanged while installing the updated
   executable. Pause new dispatch and let old workers and active drain/job
   supervisors finish, or stop them through their normal lifecycle. Keep a
   backup of the pre-change executor files.
2. Restart persistent `orbit mcp serve` processes and reconnect their clients.
   Also restart long-lived drain/workflow runners, orchestration processes,
   and web/dashboard servers that open runtimes or dispatch work. Any still-active
   sweep process or worker that can load executors or start nested Orbit commands
   must finish or restart from the updated executable. Check service/timer
   launch paths and worker `ORBIT_BIN` overrides for older executable copies.
3. Verify each restarted reader's executable provenance and run an ordinary
   read-only task/tool call through each authoritative MCP connection while the
   YAML still uses the old values. A successful new CLI `--version` invocation
   alone does not verify the executable already loaded by another process.
4. Only after all readers have been replaced, set `spec.sandbox: off`. Verify
   `orbit executor show <provider> --json`, repeat an ordinary authoritative
   MCP read, and inspect the next invocation's effective sandbox audit before
   resuming normal dispatch. If an older reader must remain active, defer the
   shared YAML change; deleting the sandbox field is not a compatibility solution.

For rollback to an older binary, pause dispatch and restore the backed-up
concrete sandbox values **before** starting any old reader. Then restart the
affected services/connections and verify ordinary reads again. Do not launch
an old MCP server or drain against shared executor files that still contain
`off`. These steps change Orbit processes and resources, not host AppArmor
settings.

### Runtime grant race regression

Runtime directory and SQLite sidecar grants are passed to Bubblewrap as held
file descriptors using `--bind-fd`; the capability probe rejects older builds
that do not advertise this option. Run the explicit kernel regression on an
authorized Linux host after building the candidate revision:

```sh
cargo test -p orbit-exec --test linux_sandbox \
  kernel_descriptor_mount_never_writes_the_replacement_object -- --ignored --exact
```

The test deliberately replaces a regular sidecar name after its descriptor is
opened and proves that the replacement is unchanged. Namespace denial is a
test failure, not a skip. Ordinary tests also cover descriptor closure and
fail-closed external-symlink replacement without requiring user namespaces.
The contract assumes concurrent writers cannot perform privileged remounts of
the already-open runtime root or its ancestors. macOS uses its existing
Seatbelt path rules; other platforms reject `linux-bwrap` at dispatch.
