# Linux sandbox onboarding

Run the shell installer or `orbit init` as the unprivileged account that will
execute Orbit. Both prepare and verify the Linux host automatically. npm
postinstall only installs the binary; `orbit init` performs preparation for
npm and direct-binary installs without a separate setup command. No privileged
operation runs during dispatch.

Orbit checks the trusted `/usr/bin/bwrap` for `--bind-fd` and runs its
namespace-and-mount probe as the intended user. A ready host is left alone.
When needed, onboarding uses the distribution package manager to install
Bubblewrap. If the host still has no Bubblewrap with `--bind-fd`, it installs
the signed, static Bubblewrap published with the Orbit release, root-owned at
`/usr/local/libexec/orbit/bwrap`; it is never setuid and never replaces a host
`bwrap` that qualifies. On Ubuntu 24.04, it loads only the packaged
`bwrap-userns-restrict` AppArmor profile, and refuses to overwrite a custom
profile. Interactive onboarding uses the normal administrator authentication
prompt. `orbit init --non-interactive` requires root or already-authorized
passwordless sudo and never waits for a password.
A failed preparation stops `orbit init` before it writes anything. When an
administrator or image build owns the host's packages, `orbit init
--skip-host-prerequisites` (or `ORBIT_SKIP_HOST_PREREQUISITES=1`) seeds Orbit
without touching the host; `linux-bwrap` dispatch stays fail-closed until
`orbit doctor providers` reports the sandbox ready.

Automatic preparation code paths: Ubuntu 24.04, Debian 13, Fedora 43–45,
Enterprise Linux 10 (`rhel`, `rocky`, `almalinux`, `centos`) and Arch. Older or
unknown versions, such as Ubuntu 22.04, get the bundled Bubblewrap when theirs
is missing or lacks `--bind-fd`, and an explicit unsupported result for any
other failure. Package availability has been checked; native package/security-policy
and sandboxed subprocess integration has **not yet been validated** for these
rows. The actual user-scoped capability probe is always the readiness gate.

Use `orbit doctor providers --json` to compare configured `sandbox` with
`sandbox_ready` and `sandbox_readiness_detail`; `sandbox_wrapper` and
`sandbox_wrapper_version` say whether the host or bundled Bubblewrap is in use.
`orbit update` refreshes an installed bundled Bubblewrap. A false readiness result can
mean missing privileges, package failure, incompatible Bubblewrap, a custom
profile conflict, AppArmor denial, or an enclosing container/kernel blocking
user namespaces. Correct the reported cause and rerun `orbit init`. Do not
weaken global namespace policy, enable `allow_fallback`, install a setuid
workaround, or switch the executor off as an automatic recovery step.

`linux-bwrap` confines writes according to the resolved policy, but host reads
and host network access remain available. Non-Linux hosts are unaffected.
