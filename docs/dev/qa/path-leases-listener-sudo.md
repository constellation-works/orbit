---
type: context
summary: "Disposable-guest recipe: a warmed sudo timestamp lets a noninteractive child become the listener user, and timestamp_timeout=0 denies that child."
last_validated: 2026-10-26
---

# Path-lease listener sudo on a disposable host

Recipe for the fresh-authorization prerequisite in
[2_design.md §7.1](../../design/path-leases/2_design.md#71-the-listener-user). The lease listener
and `orbit lease token` are not shipped. A root-owned stub stands in for the transition. The
recipe shows two results:

- Under ordinary password-protected `sudo` (`timestamp_timeout=5`), a noninteractive child in
  the same terminal reuses a warmed timestamp and runs as `orbit-lease`.
- Under the supported policy (`timestamp_timeout=0` for every `sudo` the operator can run, and
  no passwordless grant), that same child is denied. The child does not pass `sudo -k`.

A third step adds `NOPASSWD` and shows that the denial disappears. That grant is not a
supported deployment.

## Safety

Run this only in a disposable container or VM created for the recipe. Do not edit
`/etc/sudoers` or `/etc/sudoers.d` on a live owner, and do not point the recipe at a machine
that already runs Orbit.

The host command only starts the guest. Every `sudo` and every sudoers write happens inside
the guest, as the guest's users, with the password `disposable-only`. That password exists
only in the guest. Do not reuse an operator password from any real host.

The guest script refuses to continue when pid 1 is not the container shell. If that check
fails, stop. Do not override it on an owner host. `podman run --rm` (or the docker equivalent)
removes the guest on exit, which is the cleanup. Do not copy the guest sudoers fragment onto
another machine.

A pass or a fail inside the guest says nothing about a live owner's sudo policy. This recipe
does not change that policy and does not ask you to.

## Host: start the guest

`podman` or `docker` is the launcher. If neither is available, do not fall back to the host.

```sh
podman run --rm -it --name orbit-lease-sudo-qa docker.io/library/debian:stable-slim bash
```

`--rm` deletes the guest when the shell exits. Do not add `--init`: the safety check expects
the shell to be pid 1. Use `-it` so the shell has a controlling terminal. The warmed-timestamp
case is per terminal; a child started with `setsid`, or from another login, uses a different
timestamp record and hides the reuse this recipe is here to show.

## Guest: setup

Paste this as root in the guest shell. It installs `sudo`, creates the locked `orbit-lease`
account and the `operator` account, and installs the stub.

```sh
set -eu
if [ "$(cat /proc/1/comm)" != "bash" ] && [ "$(cat /proc/1/comm)" != "sh" ]; then
  echo "refusing: pid 1 is not the container shell; not writing sudoers here" >&2
  exit 1
fi
if [ "$(id -u)" -ne 0 ]; then
  echo "refusing: guest setup runs as root inside the container" >&2
  exit 1
fi
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y sudo
useradd --system --create-home --home-dir /var/lib/orbit-lease \
  --shell /usr/sbin/nologin orbit-lease
passwd -l orbit-lease
useradd --create-home --shell /bin/bash operator
printf '%s\n' 'operator:disposable-only' | chpasswd
install -d -o root -g root -m 0755 /usr/local/bin
cat > /usr/local/bin/orbit << 'EOF'
#!/bin/sh
echo orbit-lease-qa-ran
id
EOF
chown root:root /usr/local/bin/orbit
chmod 0755 /usr/local/bin/orbit
install -d -o root -g root -m 0755 /etc/sudoers.d
echo orbit-lease-qa-guest > /etc/orbit-lease-qa-guest
```

Confirm the marker before any later step: `cat /etc/orbit-lease-qa-guest` prints
`orbit-lease-qa-guest`. If it does not, stop.

## Case A — warmed timestamp, unsupported policy

Still as root in the guest, install a password-required rule with a five-minute cache. The
upstream default is 15 minutes; pinning 5 keeps the warmed timestamp inside the window
whatever default the guest package was built with. The record stays the package default,
which is one record per terminal. Do not set classic sudo's `timestamp_type=global` or turn
`tty_tickets` off in this case: that would widen the cache, and it is not the supported
policy. This fragment is ordinary password-protected `sudo`, which the design does not
accept as a boundary.

```sh
set -eu
if [ "$(cat /etc/orbit-lease-qa-guest 2>/dev/null)" != "orbit-lease-qa-guest" ]; then
  echo "refusing: missing guest marker; not writing sudoers" >&2
  exit 1
fi
cat > /etc/sudoers.d/orbit-lease-qa << 'EOF'
Defaults:operator timestamp_timeout=5
operator ALL=(orbit-lease) /usr/local/bin/orbit
EOF
chmod 0440 /etc/sudoers.d/orbit-lease-qa
visudo -cf /etc/sudoers.d/orbit-lease-qa
```

As `operator`, in this same terminal, warm the timestamp and then run the noninteractive
child. The child unsets the environment variables the design treats as agent declarations,
passes `sudo -n`, and does not pass `sudo -k`. Its stdin is `/dev/null`, so it cannot answer
a prompt.

```sh
su - operator -c 'bash -s' << 'EOF'
set -u
# Bare `tty` tests stdin, which is this heredoc. Test the controlling terminal instead.
if ! (tty </dev/tty) >/dev/null 2>&1; then
  echo "refusing: this session has no controlling terminal, so a tty timestamp cannot be shown" >&2
  exit 2
fi
printf '%s\n' 'disposable-only' | sudo -S -u orbit-lease /usr/local/bin/orbit > /tmp/warm.out
echo "warm_exit=$?"
env -u ORBIT_AGENT_NAME -u ORBIT_MANAGED_RUN_CONTEXT \
  sudo -n -u orbit-lease /usr/local/bin/orbit </dev/null > /tmp/child.out 2> /tmp/child.err
echo "child_exit=$?"
echo "child_stdout=$(cat /tmp/child.out)"
echo "child_stderr=$(cat /tmp/child.err)"
EOF
```

**Expected for case A.** `warm_exit=0` and `/tmp/warm.out` contains `orbit-lease-qa-ran`.
`child_exit=0` and the child stdout contains `orbit-lease-qa-ran`. The child ran as
`orbit-lease` with no password. That is the reuse a cached timestamp allows. It is the
unsupported result, not a deployment to copy.

If `child_exit` is non-zero here, the timestamp was not warmed for this terminal (the
controlling-terminal check failed, the warm `sudo -S` failed, or a drop-in did not override a
guest default of `timestamp_timeout=0`). Fix the guest and repeat case A. Do not treat that
failure as the supported denial, and do not change sudoers on the host to chase it.

## Case B — supported policy, unattended denial

Replace the guest fragment. `timestamp_timeout=0` applies to every `sudo` `operator` can run,
not only to the `orbit-lease` runas user. There is still no `NOPASSWD` tag and no
`!authenticate`.

```sh
set -eu
if [ "$(cat /etc/orbit-lease-qa-guest 2>/dev/null)" != "orbit-lease-qa-guest" ]; then
  echo "refusing: missing guest marker; not writing sudoers" >&2
  exit 1
fi
cat > /etc/sudoers.d/orbit-lease-qa << 'EOF'
Defaults:operator timestamp_timeout=0
operator ALL=(orbit-lease) /usr/local/bin/orbit
EOF
chmod 0440 /etc/sudoers.d/orbit-lease-qa
visudo -cf /etc/sudoers.d/orbit-lease-qa
```

Run the same warm-then-child sequence again, still in this terminal. Do not insert `sudo -k`
between the warm command and the child. The supported policy has to deny the child even when
the caller omits that flag and even when the previous `sudo` just succeeded.

```sh
su - operator -c 'bash -s' << 'EOF'
set -u
# Bare `tty` tests stdin, which is this heredoc. Test the controlling terminal instead.
if ! (tty </dev/tty) >/dev/null 2>&1; then
  echo "refusing: this session has no controlling terminal" >&2
  exit 2
fi
printf '%s\n' 'disposable-only' | sudo -S -u orbit-lease /usr/local/bin/orbit > /tmp/warm.out
echo "warm_exit=$?"
env -u ORBIT_AGENT_NAME -u ORBIT_MANAGED_RUN_CONTEXT \
  sudo -n -u orbit-lease /usr/local/bin/orbit </dev/null > /tmp/child.out 2> /tmp/child.err
echo "child_exit=$?"
echo "child_stdout=$(cat /tmp/child.out)"
echo "child_stderr=$(cat /tmp/child.err)"
EOF
```

**Expected for case B.** `warm_exit=0`: a person (here, the disposable password on stdin of
the one warm command) can still complete the transition. `child_exit` is non-zero.
`/tmp/child.out` is empty and does not contain `orbit-lease-qa-ran`. The stub did not run.
`sudo` writes a password-required denial on stderr; `sudo: a password is required` is the
usual text. The pass condition is the non-zero status and the stub not running, so a wording
change in a later `sudo` still has a defined result.

This denial is what a deployment needs before `lease.listener.bind` or
`lease.intake.peer_user` may be set. The recipe does not set either key.

## Case C — a passwordless grant defeats the denial

Still inside the guest, replace the command rule with `NOPASSWD` and keep
`timestamp_timeout=0`. This is a broader run-as grant. It is not the supported policy.

```sh
set -eu
if [ "$(cat /etc/orbit-lease-qa-guest 2>/dev/null)" != "orbit-lease-qa-guest" ]; then
  echo "refusing: missing guest marker; not writing sudoers" >&2
  exit 1
fi
cat > /etc/sudoers.d/orbit-lease-qa << 'EOF'
Defaults:operator timestamp_timeout=0
operator ALL=(orbit-lease) NOPASSWD: /usr/local/bin/orbit
EOF
chmod 0440 /etc/sudoers.d/orbit-lease-qa
visudo -cf /etc/sudoers.d/orbit-lease-qa
su - operator -c 'sudo -n -u orbit-lease /usr/local/bin/orbit' </dev/null
echo "nopasswd_child_exit=$?"
```

**Expected for case C.** `nopasswd_child_exit=0` and stdout contains `orbit-lease-qa-ran`.
No password was read. `timestamp_timeout=0` did not help, because the rule never asks.

Restore the case B fragment (the block in the previous section) and rerun the case B child.
The non-zero denial returns only after the `NOPASSWD` rule is gone. The same shape of miss
applies to `Defaults !authenticate`, a negative `timestamp_timeout`, root, `CAP_SETUID`, a
subuid map onto `orbit-lease`, and `su` / `runuser` / `doas` / `pkexec` / `systemd-run --uid`.
The recipe does not install those grants. Any one of them means the prerequisite is absent
and the listener stays off.

## What a result means

| Observation inside the guest | Meaning |
|---|---|
| Case A child exits 0 and prints `orbit-lease-qa-ran` | Cached timestamp reused. Ordinary password-protected `sudo` is not the boundary. |
| Case B child exits non-zero and does not print `orbit-lease-qa-ran` | Unattended denial under the supported policy. |
| Case C child exits 0 | A passwordless grant bypasses `timestamp_timeout=0`. Unsupported. |
| Case A child exits non-zero | The warm-up did not land on this terminal. Not evidence of the supported denial. |

Exit the guest shell. `--rm` removes it, including the sudoers fragment and the disposable
password. Leave the host's sudo policy as it was.
