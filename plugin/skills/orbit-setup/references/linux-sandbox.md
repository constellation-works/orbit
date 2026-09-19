# Prepare a Linux Host for Sandboxed Dispatch

Complete this setup after `orbit init` and before dispatching an agent on Linux.
Orbit's Linux executor uses Bubblewrap and fails closed when the host cannot
run its namespace-and-mount capability probe.

## What the Linux sandbox guarantees

The `linux-bwrap` executor resolves `/usr/bin/bwrap` and requires an
unprivileged user to create user namespaces and mounts. A failed capability
probe stops dispatch; Orbit does not silently fall back to an in-process-only
boundary.

The enforced boundary protects writes according to the resolved policy. Host
filesystem reads and host network access remain available, so this sandbox
does not provide worktree-only reads or policy-gated network egress.

## Ubuntu 24.04

Ubuntu 24.04 can restrict unprivileged user namespaces with AppArmor. Install
Bubblewrap and the distro's narrow profile, load the profile, verify that
AppArmor knows it, then run the same probe shape used by Orbit:

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

The final command must exit successfully. The profile path and probe are
specific to Ubuntu 24.04; use the corresponding packaged profile and the same
namespace-and-mount capability requirements on another Linux distribution.

## If the probe fails

Re-check that `/usr/bin/bwrap` is executable, that the packaged profile exists,
and that `apparmor_parser` loaded it. Read the parser output and rerun the
probe after correcting the host setup.

Do not disable `kernel.apparmor_restrict_unprivileged_userns` globally and do
not enable `allow_fallback`. Both changes weaken or bypass Orbit's fail-closed
boundary. Orbit supports fixing the narrow host prerequisite; it does not
support an implicit sandbox fallback.

Non-Linux hosts are unaffected by this prerequisite: macOS uses `sandbox-exec`,
and platforms without a shipped OS-level backend rely on in-process filesystem
guards only.
